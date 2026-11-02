//! Inline plugins (docs/decisions/0015-plugin-runtimes.md): a plugin's
//! `anya.inline` module runs in a QuickJS runtime of its own inside Anya,
//! given nothing of the outside world, each call within a time budget.

mod scan;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use anya_core::hooks::{
    Budgets, Call, ContextHook, Extra, HookError, HookFuture, Runs, TransformHook,
};
use anya_protocol::Provide;
use anya_protocol::Source;
use tokio::sync::oneshot;

use worker::{About, Answer, Job, Request};

/// Why an inline plugin did not load.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    /// The module file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The module uses `import` or `require`; an inline plugin is one file.
    #[error("an inline plugin is one file and cannot use `{0}`")]
    Forbidden(&'static str),
    /// The module threw while it evaluated, or is not valid.
    #[error("the module failed to load: {0}")]
    Load(String),
    /// The module spent its load budget.
    #[error("the module did not finish loading in time")]
    LoadTimeout,
    /// The module ran out of memory while loading.
    #[error("the module ran out of memory while loading")]
    LoadMemory,
    /// The module exports neither `context` nor `transform`.
    #[error("the module exports neither `context` nor `transform`")]
    NoHooks,
    /// The plugin's thread could not be started.
    #[error("cannot start the plugin's thread: {0}")]
    Thread(std::io::Error),
}

/// What every handle to one plugin shares. Dropping the last one closes
/// `jobs`, which ends the plugin's thread.
struct Shared {
    plugin: String,
    jobs: mpsc::Sender<Job>,
    provides: Vec<Provide>,
}

impl Shared {
    async fn ask(
        &self,
        request: Request,
        budget: std::time::Duration,
    ) -> Result<Answer, HookError> {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            deadline: Instant::now() + budget,
            request,
            reply,
        };
        self.jobs
            .send(job)
            .map_err(|_| HookError::Unavailable("the plugin's runtime stopped".to_owned()))?;
        answer
            .await
            .map_err(|_| HookError::Unavailable("the plugin's runtime stopped".to_owned()))?
    }
}

fn about(call: &Call) -> About {
    About {
        session: call.session.as_ref().map(|s| s.0.clone()),
        source: match call.source {
            Source::Dictation => "dictation",
            Source::Continuous => "continuous",
            Source::Unknown => "unknown",
        },
        app: call.app.clone(),
        settings: call.settings.clone(),
    }
}

/// An inline plugin: a module in a QuickJS runtime on a thread of its own.
/// Cheap to clone; the thread ends when the last clone, and the hooks taken
/// from it, are dropped.
#[derive(Clone)]
pub struct InlinePlugin {
    shared: Arc<Shared>,
}

impl InlinePlugin {
    /// Reads and evaluates `file` as the module of `plugin`, its memory
    /// limited by `budgets.inline_memory`. The module must be one file without
    /// `import` or `require` and export `context` and/or `transform`
    /// functions; it gets one second to evaluate.
    pub async fn load(
        plugin: &str,
        file: &Path,
        budgets: &Budgets,
    ) -> Result<InlinePlugin, ScriptError> {
        let (jobs, queue) = mpsc::channel();
        let (ready, loaded) = oneshot::channel();
        worker::spawn(plugin, file.to_owned(), budgets.inline_memory, ready, queue)?;
        let provides = loaded
            .await
            .map_err(|_| ScriptError::Load("the plugin's thread stopped".to_owned()))??;
        Ok(InlinePlugin {
            shared: Arc::new(Shared {
                plugin: plugin.to_owned(),
                jobs,
                provides,
            }),
        })
    }

    /// The hooks the module exports: `Context` and/or `Transform`.
    pub fn provides(&self) -> Vec<Provide> {
        self.shared.provides.clone()
    }

    /// The module's `context` hook, if it exports one.
    pub fn context(&self) -> Option<Arc<dyn ContextHook>> {
        self.shared
            .provides
            .contains(&Provide::Context)
            .then(|| Arc::new(Hook(self.shared.clone())) as Arc<dyn ContextHook>)
    }

    /// The module's `transform` hook, if it exports one.
    pub fn transform(&self) -> Option<Arc<dyn TransformHook>> {
        self.shared
            .provides
            .contains(&Provide::Transform)
            .then(|| Arc::new(Hook(self.shared.clone())) as Arc<dyn TransformHook>)
    }
}

/// One handle on a plugin, serving as whichever hook it is asked as.
struct Hook(Arc<Shared>);

impl ContextHook for Hook {
    fn plugin(&self) -> &str {
        &self.0.plugin
    }

    fn runs(&self) -> Runs {
        Runs::Inline
    }

    fn context<'a>(
        &'a self,
        call: &'a Call,
        language: Option<&'a str>,
        hotwords: &'a [String],
        budget: std::time::Duration,
    ) -> HookFuture<'a, Extra> {
        Box::pin(async move {
            let request = Request::Context {
                about: about(call),
                language: language.map(str::to_owned),
                hotwords: hotwords.to_vec(),
            };
            match self.0.ask(request, budget).await? {
                Answer::Extra(extra) => Ok(extra),
                Answer::Text(_) => {
                    Err(HookError::Failed("the runtime answered wrongly".to_owned()))
                }
            }
        })
    }
}

impl TransformHook for Hook {
    fn plugin(&self) -> &str {
        &self.0.plugin
    }

    fn runs(&self) -> Runs {
        Runs::Inline
    }

    fn transform<'a>(
        &'a self,
        text: &'a str,
        call: &'a Call,
        budget: std::time::Duration,
    ) -> HookFuture<'a, String> {
        Box::pin(async move {
            let request = Request::Transform {
                about: about(call),
                text: text.to_owned(),
            };
            match self.0.ask(request, budget).await? {
                Answer::Text(text) => Ok(text),
                Answer::Extra(_) => {
                    Err(HookError::Failed("the runtime answered wrongly".to_owned()))
                }
            }
        })
    }
}
