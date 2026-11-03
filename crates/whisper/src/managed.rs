//! The whisper.cpp engine with the settings for a server Anya runs. In
//! `managed` mode a session first takes a lease on the server, starting it
//! if needed, and then opens the engine on its address; audio that arrives
//! meanwhile waits. In `external` mode the engine is used as it is.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anya_core::engine::{Engine, EngineError, Input, Output, Session, SessionParams};
use anya_protocol::{Field, FieldKind, Manifest, Text};
use tokio::sync::mpsc;

use crate::Local;
use crate::server::{Launch, find_program};

/// The setting that says who runs the server: [`EXTERNAL`] or [`MANAGED`].
pub const MODE: &str = "mode";
/// The user runs whisper-server and names its address; the default.
pub const EXTERNAL: &str = "external";
/// Anya runs whisper-server.
pub const MANAGED: &str = "managed";
/// A catalog model's id, or a path to a ggml model file.
pub const MODEL: &str = "model";
/// The whisper-server program; empty looks it up.
pub const PROGRAM: &str = "program";
/// Minutes without a session before the server stops; `0` keeps it running.
pub const IDLE: &str = "idle_timeout_minutes";
/// The base URL models are downloaded from; empty is Hugging Face.
pub const SOURCE: &str = "download_source";
/// The setting the wrapped engine reads the server's address from.
const URL: &str = "url";

/// Minutes a managed server stays idle when the setting is empty.
const DEFAULT_IDLE_MINUTES: u64 = 10;

/// Audio a session holds while the server starts; about two minutes.
const START_INPUT: usize = 4096;

/// `engine` with the settings and sessions of a managed server.
pub(crate) struct Managed {
    inner: Arc<dyn Engine>,
    local: Arc<Local>,
    manifest: Manifest,
}

impl Managed {
    pub(crate) fn new(inner: Arc<dyn Engine>, local: Arc<Local>) -> Managed {
        let mut manifest = inner.manifest().clone();
        manifest.fields.insert(0, mode_field());
        manifest.fields.extend(managed_fields());
        Managed {
            inner,
            local,
            manifest,
        }
    }
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    settings.get(key).map_or("", |v| v.trim())
}

/// Whether `settings` ask for a managed server; an unknown mode is invalid.
fn managed(settings: &BTreeMap<String, String>) -> Result<bool, EngineError> {
    match setting(settings, MODE) {
        "" | EXTERNAL => Ok(false),
        MANAGED => Ok(true),
        _ => Err(EngineError::Invalid(MODE.to_owned())),
    }
}

impl Local {
    /// What a managed server runs for `settings`, if everything it needs is
    /// there: the program, and a model downloaded or a file that exists.
    pub(crate) fn launch(
        &self,
        settings: &BTreeMap<String, String>,
    ) -> Result<Launch, EngineError> {
        let model = match setting(settings, MODEL) {
            "" => return Err(EngineError::NotConfigured(MODEL.to_owned())),
            id if self.models.find(id).is_some() => self
                .models
                .ready(id)
                .ok_or_else(|| EngineError::NotConfigured(MODEL.to_owned()))?,
            path => {
                let path = std::path::PathBuf::from(path);
                if !path.is_file() {
                    return Err(EngineError::Invalid(MODEL.to_owned()));
                }
                path
            }
        };
        let program = setting(settings, PROGRAM);
        let program = find_program(program).ok_or_else(|| {
            if program.is_empty() {
                EngineError::NotConfigured(PROGRAM.to_owned())
            } else {
                EngineError::Invalid(PROGRAM.to_owned())
            }
        })?;
        let minutes = match setting(settings, IDLE) {
            "" => DEFAULT_IDLE_MINUTES,
            m => m
                .parse::<u64>()
                .map_err(|_| EngineError::Invalid(IDLE.to_owned()))?,
        };
        Ok(Launch {
            program,
            model,
            idle: (minutes > 0).then(|| Duration::from_secs(minutes * 60)),
        })
    }
}

impl Engine for Managed {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        if managed(settings)? {
            self.local.launch(settings).map(drop)
        } else {
            self.inner.check(settings)
        }
    }

    fn open(&self, mut params: SessionParams) -> Session {
        let launch = match managed(&params.settings) {
            Ok(false) => return self.inner.open(params),
            Ok(true) => self.local.launch(&params.settings),
            Err(e) => Err(e),
        };
        let (input, mut input_rx) = mpsc::channel::<Input>(START_INPUT);
        let (output_tx, output) = mpsc::channel::<Output>(64);
        let launch = match launch {
            Ok(launch) => launch,
            Err(e) => {
                let _ = output_tx.try_send(Output::Error(e));
                return Session { input, output };
            }
        };
        let inner = Arc::clone(&self.inner);
        let server = Arc::clone(&self.local.server);
        tokio::spawn(async move {
            let lease = match server.lease(launch).await {
                Ok(lease) => lease,
                Err(e) => {
                    let _ = output_tx
                        .send(Output::Error(EngineError::Connect(e.to_string())))
                        .await;
                    return;
                }
            };
            params.settings.insert(URL.to_owned(), lease.url.clone());
            let Session {
                input: engine_input,
                output: mut engine_output,
            } = inner.open(params);
            let pump_in = async move {
                while let Some(item) = input_rx.recv().await {
                    if engine_input.send(item).await.is_err() {
                        break;
                    }
                }
                // Dropping engine_input here passes an abandoned session on.
            };
            let pump_out = async move {
                while let Some(item) = engine_output.recv().await {
                    if output_tx.send(item).await.is_err() {
                        break;
                    }
                }
                // The session is over once its output is: the server is free.
                drop(lease);
            };
            tokio::join!(pump_in, pump_out);
        });
        Session { input, output }
    }
}

fn field(key: &str, label: [&str; 2], help: [&str; 2], kind: FieldKind, advanced: bool) -> Field {
    Field {
        key: key.to_owned(),
        label: Text::localized([("en", label[0]), ("zh-CN", label[1])]),
        help: Text::localized([("en", help[0]), ("zh-CN", help[1])]),
        kind,
        required: false,
        advanced,
    }
}

fn mode_field() -> Field {
    field(
        MODE,
        ["Who runs the server", "运行方式"],
        [
            "external: you start whisper-server and Anya connects to its address; managed: Anya starts it on this computer when a session needs it and stops it when idle",
            "external：你自己启动 whisper-server，Anya 连它的地址；managed：需要时由 Anya 在本机启动，空闲后退出",
        ],
        FieldKind::Choice {
            options: vec![EXTERNAL.to_owned(), MANAGED.to_owned()],
        },
        false,
    )
}

fn managed_fields() -> [Field; 4] {
    [
        field(
            MODEL,
            ["Model", "模型"],
            [
                "When Anya runs the server: a downloaded model (base, small, large-v3-turbo-q5_0, large-v3-turbo, large-v3) or the path of a ggml model file",
                "由 Anya 启动时使用：已下载的模型（base、small、large-v3-turbo-q5_0、large-v3-turbo、large-v3）或 ggml 模型文件的路径",
            ],
            FieldKind::Text,
            false,
        ),
        field(
            PROGRAM,
            ["Program", "程序"],
            [
                "When Anya runs the server; empty looks for whisper-server on PATH and in Homebrew",
                "由 Anya 启动时使用；留空则在 PATH 和 Homebrew 里找 whisper-server",
            ],
            FieldKind::Text,
            true,
        ),
        field(
            IDLE,
            ["Stop when idle (minutes)", "空闲后退出（分钟）"],
            [
                "Frees the model's memory; the next session loads it again. 0 keeps it running; empty is 10",
                "退出后释放模型占的内存，下次听写时重新加载。0 表示一直运行，留空为 10",
            ],
            FieldKind::Text,
            true,
        ),
        field(
            SOURCE,
            ["Download source", "下载源"],
            [
                "Where models are downloaded from; empty is Hugging Face. Every file is checked against its pinned hash, whatever the source",
                "下载模型的地址，留空为 Hugging Face。无论从哪里下载，文件都要通过固定的哈希校验",
            ],
            FieldKind::Text,
            true,
        ),
    ]
}
