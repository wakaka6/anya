//! The hooks around a local dictation (docs/decisions/0016-dictation-hooks.md):
//! `context` before the engine opens, `transform` on the final text, and
//! `action` beside the paste. Each is a seam with built-in, inline (QuickJS)
//! and host (Bun) adapters; this module also runs them, with the budgets of
//! docs/decisions/0015-plugin-runtimes.md and each hook's failure rule.

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anya_protocol::{Limits, Manifest, Provide, SessionId, Source};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::engine::{Engine, EngineError, Input, Output, Session, SessionParams};

/// What a hook call returns.
pub type HookFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HookError>> + Send + 'a>>;

/// Where a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runs {
    /// Compiled into Anya.
    Builtin,
    /// In QuickJS inside Anya: no network, no disk.
    Inline,
    /// In the plugin's own Bun process.
    Host,
}

/// What a hook call is about. Every hook gets its own copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The session; `None` for a context call, made before it opens.
    pub session: Option<SessionId>,
    /// What opened the session.
    pub source: Source,
    /// The focused app's id, when the desktop tells it.
    pub app: Option<String>,
    /// The plugin's settings, keyed by the field keys its package declares.
    pub settings: BTreeMap<String, String>,
}

/// What a context hook adds to a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extra {
    /// Words to add to the session's hotwords.
    pub hotwords: Vec<String>,
    /// The language to open the session with.
    pub language: Option<String>,
}

/// Why a hook call failed. The Display text is for logs and may hold the
/// plugin's own words; never a transcript.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// The call's budget was spent.
    #[error("the hook ran out of time")]
    Timeout,
    /// An inline plugin went over its memory limit.
    #[error("the hook ran out of memory")]
    Memory,
    /// The hook answered with the wrong shape; says what was wrong.
    #[error("the hook answered with the wrong shape: {0}")]
    Invalid(String),
    /// The hook threw or rejected; the plugin's own words.
    #[error("the hook failed: {0}")]
    Failed(String),
    /// The plugin is not running; says why.
    #[error("the plugin is not running: {0}")]
    Unavailable(String),
}

impl HookError {
    /// The fixed code kept in diagnostics and shown translated.
    pub fn failure(&self) -> HookFailure {
        match self {
            HookError::Timeout => HookFailure::Timeout,
            HookError::Memory => HookFailure::Memory,
            HookError::Invalid(_) => HookFailure::Invalid,
            HookError::Failed(_) => HookFailure::Failed,
            HookError::Unavailable(_) => HookFailure::Unavailable,
        }
    }
}

/// A hook failure as a code: what the activity list keeps and UIs translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFailure {
    /// The budget was spent.
    Timeout,
    /// An inline plugin went over its memory limit.
    Memory,
    /// The answer had the wrong shape, or a transform's text was too long.
    Invalid,
    /// The hook threw or rejected.
    Failed,
    /// The plugin was not running.
    Unavailable,
    /// The plugin's actions were too far behind; this one was dropped.
    Busy,
    /// A code a newer Anya wrote.
    #[serde(other)]
    Unknown,
}

/// One failed hook call, as the caller reports it. No text of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailed {
    /// The plugin that provides the hook.
    pub plugin: String,
    /// Which hook.
    pub hook: Provide,
    /// Why, as a code.
    pub failure: HookFailure,
    /// The plugin's or the runtime's own words, for the log.
    pub detail: String,
}

/// Adds hotwords and a language to a dictation before it opens.
pub trait ContextHook: Send + Sync {
    /// The plugin that provides it.
    fn plugin(&self) -> &str;
    /// Where it runs.
    fn runs(&self) -> Runs;
    /// What the session should open with besides `language` and `hotwords`.
    /// `budget` is the time the call has; the adapter stops the plugin when
    /// it is spent.
    fn context<'a>(
        &'a self,
        call: &'a Call,
        language: Option<&'a str>,
        hotwords: &'a [String],
        budget: Duration,
    ) -> HookFuture<'a, Extra>;
}

/// Text in, text out, on a dictation's final text.
pub trait TransformHook: Send + Sync {
    /// The plugin that provides it.
    fn plugin(&self) -> &str;
    /// Where it runs.
    fn runs(&self) -> Runs;
    /// The text that goes on.
    fn transform<'a>(
        &'a self,
        text: &'a str,
        call: &'a Call,
        budget: Duration,
    ) -> HookFuture<'a, String>;
}

/// Does something with a dictation's final text, beside the paste.
pub trait ActionHook: Send + Sync {
    /// The plugin that provides it.
    fn plugin(&self) -> &str;
    /// Where it runs.
    fn runs(&self) -> Runs;
    /// Acts on `text`.
    fn act<'a>(&'a self, text: &'a str, call: &'a Call, budget: Duration) -> HookFuture<'a, ()>;
}

/// The most UTF-8 bytes a transform may answer with.
pub const MAX_TRANSFORM_BYTES: usize = 64 << 10;

/// One budget's default and range, in its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// When nothing asks for another value.
    pub default: u32,
    /// The least a user or a plugin may set.
    pub min: u32,
    /// The most; not configurable.
    pub max: u32,
}

impl Range {
    /// `asked`, clamped; the default when absent.
    pub fn clamp(self, asked: Option<u32>) -> u32 {
        asked.unwrap_or(self.default).clamp(self.min, self.max)
    }
}

/// Milliseconds an inline call may run.
pub const INLINE_MS: Range = Range {
    default: 20,
    min: 1,
    max: 200,
};
/// MiB an inline plugin's runtime may hold.
pub const INLINE_MEMORY_MIB: Range = Range {
    default: 16,
    min: 4,
    max: 256,
};
/// Milliseconds a host context call may take.
pub const CONTEXT_MS: Range = Range {
    default: 300,
    min: 50,
    max: 2000,
};
/// Milliseconds a host transform call may take.
pub const TRANSFORM_MS: Range = Range {
    default: 2000,
    min: 100,
    max: 10000,
};
/// Milliseconds the whole transform chain may take.
pub const CHAIN_MS: Range = Range {
    default: 4000,
    min: 500,
    max: 15000,
};
/// Milliseconds an action may take.
pub const ACTION_MS: Range = Range {
    default: 10000,
    min: 1000,
    max: 60000,
};

/// One plugin's budgets, every value clamped to its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// An inline call.
    pub inline: Duration,
    /// An inline runtime's memory, in bytes.
    pub inline_memory: usize,
    /// A host context call.
    pub context: Duration,
    /// A host transform call.
    pub transform: Duration,
    /// An action.
    pub action: Duration,
}

impl Budgets {
    /// The budgets of a plugin: the user's for this plugin, else the user's
    /// for every plugin, else what the plugin's package asks for, else the
    /// default; then clamped.
    pub fn resolve(own: Limits, every: Limits, asked: Limits) -> Budgets {
        let l = own.or(every).or(asked);
        let ms = |r: Range, v| Duration::from_millis(u64::from(r.clamp(v)));
        Budgets {
            inline: ms(INLINE_MS, l.inline_ms),
            inline_memory: (INLINE_MEMORY_MIB.clamp(l.inline_memory_mib) as usize) << 20,
            context: ms(CONTEXT_MS, l.context_ms),
            transform: ms(TRANSFORM_MS, l.transform_ms),
            action: ms(ACTION_MS, l.action_ms),
        }
    }

    /// The budget of one call of `hook` running in `runs`.
    pub fn of(&self, hook: Provide, runs: Runs) -> Duration {
        match (hook, runs) {
            (Provide::Action, _) => self.action,
            (_, Runs::Inline) => self.inline,
            (Provide::Context, _) => self.context,
            _ => self.transform,
        }
    }
}

/// The whole transform chain's budget: `asked`, clamped.
pub fn chain_budget(asked: Option<u32>) -> Duration {
    Duration::from_millis(u64::from(CHAIN_MS.clamp(asked)))
}

/// A hook as one dictation runs it: its settings and its budget.
pub struct Planned<H: ?Sized> {
    /// The hook.
    pub hook: Arc<H>,
    /// The plugin's settings.
    pub settings: BTreeMap<String, String>,
    /// The time one call has.
    pub budget: Duration,
}

impl<H: ?Sized> Clone for Planned<H> {
    fn clone(&self) -> Self {
        Planned {
            hook: Arc::clone(&self.hook),
            settings: self.settings.clone(),
            budget: self.budget,
        }
    }
}

/// How long past its budget a hook's adapter has to give up by itself before
/// the runner gives up on it.
const GRACE: Duration = Duration::from_millis(50);

/// `call` with the plugin's settings.
fn with_settings(call: &Call, settings: &BTreeMap<String, String>) -> Call {
    Call {
        settings: settings.clone(),
        ..call.clone()
    }
}

async fn bounded<T>(
    budget: Duration,
    fut: impl Future<Output = Result<T, HookError>>,
) -> Result<T, HookError> {
    match tokio::time::timeout(budget + GRACE, fut).await {
        Ok(r) => r,
        Err(_) => Err(HookError::Timeout),
    }
}

fn failed(plugin: &str, hook: Provide, e: &HookError) -> HookFailed {
    HookFailed {
        plugin: plugin.to_owned(),
        hook,
        failure: e.failure(),
        detail: e.to_string(),
    }
}

/// Asks every context hook at once and merges their answers into `params`
/// in the hooks' order: hotwords are added once each, and the first hook
/// that names a language sets it. A hook that fails adds nothing.
pub async fn gather(
    hooks: &[Planned<dyn ContextHook>],
    call: &Call,
    params: &mut SessionParams,
) -> Vec<HookFailed> {
    let mut tasks = Vec::with_capacity(hooks.len());
    for planned in hooks {
        let planned = planned.clone();
        let call = with_settings(call, &planned.settings);
        let language = params.language.clone();
        let hotwords = params.hotwords.clone();
        tasks.push(tokio::spawn(async move {
            let hook = &planned.hook;
            bounded(
                planned.budget,
                hook.context(&call, language.as_deref(), &hotwords, planned.budget),
            )
            .await
        }));
    }
    let mut failures = Vec::new();
    let mut language = None;
    for (planned, task) in hooks.iter().zip(tasks) {
        let answer = task
            .await
            .unwrap_or_else(|e| Err(HookError::Failed(e.to_string())));
        match answer {
            Ok(extra) => {
                for word in extra.hotwords {
                    let word = word.trim();
                    if !word.is_empty() && !params.hotwords.iter().any(|w| w == word) {
                        params.hotwords.push(word.to_owned());
                    }
                }
                if language.is_none() {
                    language = extra.language.filter(|l| !l.trim().is_empty());
                }
            }
            Err(e) => failures.push(failed(planned.hook.plugin(), Provide::Context, &e)),
        }
    }
    if language.is_some() {
        params.language = language;
    }
    failures
}

/// Runs the transforms in order on `text`, the whole chain within `total`.
/// A step that fails, or that the spent chain budget leaves no time for, is
/// skipped and the text before it goes on.
pub async fn transform(
    chain: &[Planned<dyn TransformHook>],
    mut text: String,
    call: &Call,
    total: Duration,
) -> (String, Vec<HookFailed>) {
    let end = Instant::now() + total;
    let mut failures = Vec::new();
    for planned in chain {
        let hook = &planned.hook;
        let left = end.saturating_duration_since(Instant::now());
        let answer = if left.is_zero() {
            Err(HookError::Timeout)
        } else {
            let budget = planned.budget.min(left);
            let call = with_settings(call, &planned.settings);
            bounded(budget, hook.transform(&text, &call, budget))
                .await
                .and_then(|out| {
                    if out.len() > MAX_TRANSFORM_BYTES {
                        Err(HookError::Invalid(format!(
                            "{} bytes of text, over {MAX_TRANSFORM_BYTES}",
                            out.len()
                        )))
                    } else {
                        Ok(out)
                    }
                })
        };
        match answer {
            Ok(out) => text = out,
            Err(e) => failures.push(failed(hook.plugin(), Provide::Transform, &e)),
        }
    }
    (text, failures)
}

/// Actions a plugin may have waiting before more are dropped as busy.
const ACTION_QUEUE: usize = 32;

struct Job {
    planned: Planned<dyn ActionHook>,
    text: String,
    call: Call,
}

/// Runs actions off the paste's path: each plugin's in session order, one
/// at a time, and different plugins' side by side. Failures arrive on the
/// receiver [`Actions::new`] returns.
pub struct Actions {
    queues: Mutex<HashMap<String, mpsc::Sender<Job>>>,
    failed: mpsc::UnboundedSender<HookFailed>,
}

impl Actions {
    /// No queue yet; one starts with a plugin's first action.
    pub fn new() -> (Actions, mpsc::UnboundedReceiver<HookFailed>) {
        let (failed, rx) = mpsc::unbounded_channel();
        (
            Actions {
                queues: Mutex::new(HashMap::new()),
                failed,
            },
            rx,
        )
    }

    /// Queues `text` for every action and returns at once. Must be called
    /// inside a Tokio runtime.
    pub fn dispatch(&self, actions: &[Planned<dyn ActionHook>], text: &str, call: &Call) {
        let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        for planned in actions {
            let plugin = planned.hook.plugin().to_owned();
            let job = Job {
                planned: planned.clone(),
                text: text.to_owned(),
                call: with_settings(call, &planned.settings),
            };
            let queue = queues
                .entry(plugin.clone())
                .or_insert_with(|| self.worker());
            let job = match queue.try_send(job) {
                Ok(()) => continue,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let _ = self.failed.send(HookFailed {
                        plugin,
                        hook: Provide::Action,
                        failure: HookFailure::Busy,
                        detail: format!("{ACTION_QUEUE} actions are still waiting"),
                    });
                    continue;
                }
                Err(mpsc::error::TrySendError::Closed(job)) => job,
            };
            // The worker ended (its runtime went away): start another.
            let fresh = self.worker();
            let _ = fresh.try_send(job);
            queues.insert(plugin, fresh);
        }
    }

    fn worker(&self) -> mpsc::Sender<Job> {
        let (tx, mut rx) = mpsc::channel::<Job>(ACTION_QUEUE);
        let report = self.failed.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let hook = &job.planned.hook;
                let budget = job.planned.budget;
                if let Err(e) = bounded(budget, hook.act(&job.text, &job.call, budget)).await {
                    let _ = report.send(failed(hook.plugin(), Provide::Action, &e));
                }
            }
        });
        tx
    }
}

/// Audio a [`ContextEngine`] session holds while its context hooks answer;
/// the same as a session's own input buffer.
const CONTEXT_INPUT: usize = 4096;

/// An engine whose sessions ask context hooks before they open. Audio that
/// arrives meanwhile waits; the engine itself opens with the merged params.
pub struct ContextEngine {
    inner: Arc<dyn Engine>,
    hooks: Vec<Planned<dyn ContextHook>>,
    call: Call,
    failed: Arc<dyn Fn(Vec<HookFailed>) + Send + Sync>,
}

impl ContextEngine {
    /// `inner`, asking `hooks` about `call` before each open; `failed` is
    /// told of the hooks that failed.
    pub fn new(
        inner: Arc<dyn Engine>,
        hooks: Vec<Planned<dyn ContextHook>>,
        call: Call,
        failed: Arc<dyn Fn(Vec<HookFailed>) + Send + Sync>,
    ) -> ContextEngine {
        ContextEngine {
            inner,
            hooks,
            call,
            failed,
        }
    }
}

impl Engine for ContextEngine {
    fn manifest(&self) -> &Manifest {
        self.inner.manifest()
    }

    fn check(&self, settings: &BTreeMap<String, String>) -> Result<(), EngineError> {
        self.inner.check(settings)
    }

    fn open(&self, mut params: SessionParams) -> Session {
        let (input, mut input_rx) = mpsc::channel::<Input>(CONTEXT_INPUT);
        let (output_tx, output) = mpsc::channel::<Output>(64);
        let inner = Arc::clone(&self.inner);
        let hooks = self.hooks.clone();
        let call = self.call.clone();
        let failed = Arc::clone(&self.failed);
        tokio::spawn(async move {
            let failures = gather(&hooks, &call, &mut params).await;
            if !failures.is_empty() {
                failed(failures);
            }
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
            };
            tokio::join!(pump_in, pump_out);
        });
        Session { input, output }
    }
}
