//! A plugin's engine as the rest of Anya sees it: one more [`Engine`].

use std::sync::Arc;

use anya_core::audio::Chunk;
use anya_core::engine::{Engine, EngineError, Input, Output, Session, SessionParams};
use anya_protocol::{HostCommand, Manifest};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio::sync::mpsc;

use crate::link::{Link, OUTPUT_QUEUE};

/// Audio a session holds before the plugin takes it; about two minutes, as
/// for built-in engines.
const INPUT_CAPACITY: usize = 4096;

/// An engine that runs in the plugin host. Sessions map to `open`, `audio`,
/// `finish` and `close` commands; what the plugin reports comes back as
/// [`Output`]s. When the host process dies the session ends with an error.
pub struct PluginEngine {
    plugin: String,
    manifest: Manifest,
    link: Arc<Link>,
}

impl PluginEngine {
    pub(crate) fn new(plugin: String, manifest: Manifest, link: Arc<Link>) -> PluginEngine {
        PluginEngine {
            plugin,
            manifest,
            link,
        }
    }

    /// The id of the plugin package that provides this engine.
    pub fn plugin(&self) -> &str {
        &self.plugin
    }
}

impl Engine for PluginEngine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn open(&self, params: SessionParams) -> Session {
        let (input, audio) = mpsc::channel(INPUT_CAPACITY);
        let (results, output) = mpsc::channel(OUTPUT_QUEUE);
        let Some((id, writer)) = self.link.register(results.clone()) else {
            let _ = results.try_send(Output::Error(EngineError::Connect(
                "the plugin host is not running".to_owned(),
            )));
            return Session { input, output };
        };
        let open = HostCommand::Open {
            session: id.clone(),
            plugin: self.plugin.clone(),
            engine: self.manifest.id.clone(),
            language: params.language,
            hotwords: params.hotwords,
            settings: params.settings,
        };
        if writer.try_send(open).is_err() {
            self.link.fail_session(
                &id,
                EngineError::Upstream("the plugin host is not keeping up".to_owned()),
            );
            return Session { input, output };
        }
        tokio::spawn(forward(Arc::clone(&self.link), id, writer, audio));
        Session { input, output }
    }
}

/// Carries a session's audio to the host. It never blocks the session's
/// owner: audio the host cannot take ends the session instead.
async fn forward(
    link: Arc<Link>,
    id: String,
    writer: mpsc::Sender<HostCommand>,
    mut audio: mpsc::Receiver<Input>,
) {
    while let Some(input) = audio.recv().await {
        match input {
            Input::Audio(Chunk(pcm)) => {
                let command = HostCommand::Audio {
                    session: id.clone(),
                    pcm: STANDARD.encode(pcm),
                };
                if writer.try_send(command).is_err() {
                    link.fail_session(
                        &id,
                        EngineError::Upstream("the plugin host is not keeping up".to_owned()),
                    );
                    return;
                }
            }
            Input::Finish => {
                let _ = writer.send(HostCommand::Finish { session: id }).await;
                return;
            }
        }
    }
    // The owner dropped the session without finishing it: abandoned.
    if link.take_session(&id) {
        let _ = writer.send(HostCommand::Close { session: id }).await;
    }
}
