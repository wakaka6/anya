//! A plugin's context, transform and action hooks as the rest of Anya sees
//! them: calls to its Bun process, each with a call id, a budget and a
//! `cancel` when the budget is spent.

use std::sync::Arc;
use std::time::Duration;

use anya_core::hooks::{
    ActionHook, Call, ContextHook, Extra, HookError, HookFuture, Runs, TransformHook,
};
use anya_protocol::{HookCall, HostCommand};
use tokio::sync::mpsc;

use crate::link::{Answer, Link};

/// The hooks of one plugin that runs in its own Bun process. One value
/// serves as the plugin's [`ContextHook`], [`TransformHook`] and
/// [`ActionHook`]; the plugin answers a call for a hook it does not provide
/// with a failure.
pub struct HostHook {
    link: Arc<Link>,
}

/// Forgets the call and tells the plugin to stop it, unless it was answered.
struct Pending<'a> {
    link: &'a Link,
    id: String,
    writer: mpsc::Sender<HostCommand>,
    answered: bool,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.answered {
            self.link.forget_call(&self.id);
            let _ = self.writer.try_send(HostCommand::Cancel {
                call: self.id.clone(),
            });
        }
    }
}

impl HostHook {
    pub(crate) fn new(link: Arc<Link>) -> HostHook {
        HostHook { link }
    }

    fn about(call: &Call) -> HookCall {
        HookCall {
            session: call.session.as_ref().map(|s| s.0.clone()),
            source: call.source,
            app: call.app.clone(),
            settings: call.settings.clone(),
        }
    }

    /// Sends the command `make` builds for a fresh call id and waits for the
    /// answer for `budget`. Past it the plugin is told to `cancel`, and the
    /// answer, should it still come, is dropped.
    async fn ask(
        &self,
        budget: Duration,
        make: impl FnOnce(String) -> HostCommand,
    ) -> Result<Answer, HookError> {
        let not_running = || HookError::Unavailable("the plugin is not running".to_owned());
        let call = self.link.begin_call().ok_or_else(not_running)?;
        let mut pending = Pending {
            link: &self.link,
            id: call.id.clone(),
            writer: call.writer.clone(),
            answered: false,
        };
        if call.writer.try_send(make(call.id)).is_err() {
            pending.answered = true;
            self.link.forget_call(&pending.id);
            return Err(HookError::Unavailable(
                "the plugin host is not keeping up".to_owned(),
            ));
        }
        match tokio::time::timeout(budget, call.answer).await {
            Ok(Ok(result)) => {
                pending.answered = true;
                result
            }
            Ok(Err(_)) => {
                pending.answered = true;
                Err(not_running())
            }
            Err(_) => Err(HookError::Timeout),
        }
    }
}

fn wrong(what: &str) -> HookError {
    HookError::Invalid(format!("the plugin answered with {what}"))
}

impl ContextHook for HostHook {
    fn plugin(&self) -> &str {
        self.link.plugin()
    }

    fn runs(&self) -> Runs {
        Runs::Host
    }

    fn context<'a>(
        &'a self,
        call: &'a Call,
        language: Option<&'a str>,
        hotwords: &'a [String],
        budget: Duration,
    ) -> HookFuture<'a, Extra> {
        Box::pin(async move {
            let answer = self
                .ask(budget, |id| HostCommand::Context {
                    call: id,
                    plugin: self.link.plugin().to_owned(),
                    about: HostHook::about(call),
                    language: language.map(str::to_owned),
                    hotwords: hotwords.to_vec(),
                })
                .await?;
            match answer {
                Answer::Context(extra) => Ok(extra),
                _ => Err(wrong("something other than context")),
            }
        })
    }
}

impl TransformHook for HostHook {
    fn plugin(&self) -> &str {
        self.link.plugin()
    }

    fn runs(&self) -> Runs {
        Runs::Host
    }

    fn transform<'a>(
        &'a self,
        text: &'a str,
        call: &'a Call,
        budget: Duration,
    ) -> HookFuture<'a, String> {
        Box::pin(async move {
            let answer = self
                .ask(budget, |id| HostCommand::Transform {
                    call: id,
                    plugin: self.link.plugin().to_owned(),
                    text: text.to_owned(),
                    about: HostHook::about(call),
                })
                .await?;
            match answer {
                Answer::Text(text) => Ok(text),
                _ => Err(wrong("something other than text")),
            }
        })
    }
}

impl ActionHook for HostHook {
    fn plugin(&self) -> &str {
        self.link.plugin()
    }

    fn runs(&self) -> Runs {
        Runs::Host
    }

    fn act<'a>(&'a self, text: &'a str, call: &'a Call, budget: Duration) -> HookFuture<'a, ()> {
        Box::pin(async move {
            let answer = self
                .ask(budget, |id| HostCommand::Action {
                    call: id,
                    plugin: self.link.plugin().to_owned(),
                    text: text.to_owned(),
                    about: HostHook::about(call),
                })
                .await?;
            match answer {
                Answer::Done => Ok(()),
                _ => Err(wrong("something other than done")),
            }
        })
    }
}
