//! The thread an inline plugin's QuickJS runtime lives on. rquickjs types are
//! not `Send`, so the runtime is built, used and dropped on this one thread;
//! calls come in over a channel and answers go back over oneshots.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anya_core::hooks::{Extra, HookError};
use anya_protocol::Provide;
use rquickjs::function::Rest;
use rquickjs::{CaughtError, Context, Ctx, Function, Module, Object, Persistent, Runtime, Value};
use tokio::sync::oneshot;

use crate::{ScriptError, scan};

/// The stack a plugin's runtime may use, in bytes.
const STACK_BYTES: usize = 512 << 10;

/// The time a module has to evaluate when it loads.
pub(crate) const LOAD_BUDGET: Duration = Duration::from_secs(1);

/// What a hook's `ctx` argument is made of; the same for every hook, the
/// fields a hook is not given are left out when it is built.
pub(crate) struct About {
    pub(crate) session: Option<String>,
    pub(crate) source: &'static str,
    pub(crate) app: Option<String>,
    pub(crate) settings: BTreeMap<String, String>,
}

/// One call to a plugin's hook.
pub(crate) enum Request {
    /// `context(ctx)`.
    Context {
        about: About,
        language: Option<String>,
        hotwords: Vec<String>,
    },
    /// `transform(text, ctx)`.
    Transform { about: About, text: String },
}

/// What a call answers with.
pub(crate) enum Answer {
    Extra(Extra),
    Text(String),
}

/// A call, its deadline and where its answer goes.
pub(crate) struct Job {
    pub(crate) deadline: Instant,
    pub(crate) request: Request,
    pub(crate) reply: oneshot::Sender<Result<Answer, HookError>>,
}

/// The hooks a loaded plugin provides, once it has loaded.
pub(crate) type Loaded = Result<Vec<Provide>, ScriptError>;

/// Why a call into the runtime ended without an answer.
enum Fault {
    Timeout,
    Memory,
    Invalid(String),
    Failed(String),
}

impl From<Fault> for HookError {
    fn from(fault: Fault) -> HookError {
        match fault {
            Fault::Timeout => HookError::Timeout,
            Fault::Memory => HookError::Memory,
            Fault::Invalid(detail) => HookError::Invalid(detail),
            Fault::Failed(detail) => HookError::Failed(detail),
        }
    }
}

/// The deadline the interrupt handler holds a call to.
#[derive(Clone)]
struct Guard {
    base: Instant,
    deadline_ns: Arc<AtomicU64>,
    tripped: Arc<AtomicBool>,
}

impl Guard {
    fn new() -> Guard {
        Guard {
            base: Instant::now(),
            deadline_ns: Arc::new(AtomicU64::new(u64::MAX)),
            tripped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn arm(&self, deadline: Instant) {
        let ns = deadline.saturating_duration_since(self.base).as_nanos();
        self.tripped.store(false, Ordering::Relaxed);
        self.deadline_ns
            .store(u64::try_from(ns).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    fn disarm(&self) {
        self.deadline_ns.store(u64::MAX, Ordering::Relaxed);
    }

    fn expired(&self) -> bool {
        let ns = self.base.elapsed().as_nanos();
        u64::try_from(ns).unwrap_or(u64::MAX) > self.deadline_ns.load(Ordering::Relaxed)
    }

    /// Whether the interrupt handler stopped the running call.
    fn stopped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }
}

/// The functions a module exports.
struct Hooks {
    context: Option<Persistent<Function<'static>>>,
    transform: Option<Persistent<Function<'static>>>,
}

/// Starts the thread: it reads and loads `file`, reports on `ready`, then
/// serves `jobs` until the channel closes.
pub(crate) fn spawn(
    plugin: &str,
    file: PathBuf,
    memory: usize,
    ready: oneshot::Sender<Loaded>,
    jobs: mpsc::Receiver<Job>,
) -> Result<(), ScriptError> {
    let plugin = plugin.to_owned();
    std::thread::Builder::new()
        .name(format!("anya-script-{plugin}"))
        .spawn(move || serve(&plugin, &file, memory, ready, &jobs))
        .map(drop)
        .map_err(ScriptError::Thread)
}

fn serve(
    plugin: &str,
    file: &std::path::Path,
    memory: usize,
    ready: oneshot::Sender<Loaded>,
    jobs: &mpsc::Receiver<Job>,
) {
    let guard = Guard::new();
    let (runtime, context) = match start(memory, &guard) {
        Ok(started) => started,
        Err(e) => {
            let _ = ready.send(Err(ScriptError::Load(e.to_string())));
            return;
        }
    };
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(source) => {
            let path = file.to_owned();
            let _ = ready.send(Err(ScriptError::Read { path, source }));
            return;
        }
    };
    if let Some(word) = scan::forbidden(&source) {
        let _ = ready.send(Err(ScriptError::Forbidden(word)));
        return;
    }

    guard.arm(Instant::now() + LOAD_BUDGET);
    let loaded = context.with(|ctx| load(&ctx, plugin, source, &guard));
    guard.disarm();
    let hooks = match loaded {
        Ok(hooks) => hooks,
        Err(fault) => {
            let _ = ready.send(Err(match fault {
                Fault::Timeout => ScriptError::LoadTimeout,
                Fault::Memory => ScriptError::LoadMemory,
                Fault::Invalid(detail) | Fault::Failed(detail) => ScriptError::Load(detail),
            }));
            return;
        }
    };
    let mut provides = Vec::new();
    if hooks.context.is_some() {
        provides.push(Provide::Context);
    }
    if hooks.transform.is_some() {
        provides.push(Provide::Transform);
    }
    if provides.is_empty() {
        let _ = ready.send(Err(ScriptError::NoHooks));
        return;
    }
    if ready.send(Ok(provides)).is_err() {
        return;
    }

    while let Ok(job) = jobs.recv() {
        if job.reply.is_closed() {
            continue;
        }
        guard.arm(job.deadline);
        let out = context.with(|ctx| call(&ctx, &hooks, &guard, job.request));
        guard.disarm();
        if matches!(out, Err(Fault::Memory)) {
            runtime.run_gc();
        }
        let _ = job.reply.send(out.map_err(HookError::from));
    }
}

fn start(memory: usize, guard: &Guard) -> rquickjs::Result<(Runtime, Context)> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(memory);
    runtime.set_max_stack_size(STACK_BYTES);
    let g = guard.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let stop = g.expired();
        if stop {
            g.tripped.store(true, Ordering::Relaxed);
        }
        stop
    })));
    let context = Context::full(&runtime)?;
    Ok((runtime, context))
}

fn load(ctx: &Ctx<'_>, plugin: &str, source: String, guard: &Guard) -> Result<Hooks, Fault> {
    let fail = |e| fault(ctx, guard, e);
    console(ctx, plugin).map_err(fail)?;
    let (module, promise) = Module::declare(ctx.clone(), "plugin", source)
        .and_then(|declared| declared.eval())
        .map_err(fail)?;
    promise.finish::<()>().map_err(fail)?;
    let exports = module.namespace().map_err(fail)?;
    Ok(Hooks {
        context: export(ctx, &exports, "context", guard)?,
        transform: export(ctx, &exports, "transform", guard)?,
    })
}

fn export<'js>(
    ctx: &Ctx<'js>,
    exports: &Object<'js>,
    name: &str,
    guard: &Guard,
) -> Result<Option<Persistent<Function<'static>>>, Fault> {
    let value: Value = exports.get(name).map_err(|e| fault(ctx, guard, e))?;
    if value.is_undefined() {
        return Ok(None);
    }
    match value.into_function() {
        Some(function) => Ok(Some(Persistent::save(ctx, function))),
        None => Err(Fault::Failed(format!(
            "the export `{name}` is not a function"
        ))),
    }
}

/// A `console` whose methods write to the debug log under the plugin's id:
/// the only thing a module can reach outside its runtime.
fn console<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<()> {
    let console = Object::new(ctx.clone())?;
    for level in ["debug", "log", "info", "warn", "error"] {
        let plugin = plugin.to_owned();
        let method = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
            let line = args
                .0
                .iter()
                .map(|arg| text_of(&ctx, arg))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::debug!(%plugin, level, "{line}");
        })?;
        console.set(level, method)?;
    }
    ctx.globals().set("console", console)
}

fn text_of<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> String {
    if let Some(s) = value.as_string() {
        return s.to_string().unwrap_or_default();
    }
    match ctx.json_stringify(value.clone()) {
        Ok(Some(json)) => json.to_string().unwrap_or_default(),
        _ => String::new(),
    }
}

/// What went wrong, in the plugin's own words where it has some.
fn fault(ctx: &Ctx<'_>, guard: &Guard, error: rquickjs::Error) -> Fault {
    if guard.stopped() {
        return Fault::Timeout;
    }
    if matches!(error, rquickjs::Error::Allocation) {
        return Fault::Memory;
    }
    let message = match CaughtError::from_error(ctx, error) {
        CaughtError::Exception(e) => e.message().unwrap_or_default(),
        CaughtError::Value(v) => text_of(ctx, &v),
        CaughtError::Error(e) => e.to_string(),
    };
    if message.contains("out of memory") {
        Fault::Memory
    } else if message.is_empty() {
        Fault::Failed("the plugin threw an error".to_owned())
    } else {
        Fault::Failed(message)
    }
}

fn call(ctx: &Ctx<'_>, hooks: &Hooks, guard: &Guard, request: Request) -> Result<Answer, Fault> {
    let fail = |e| fault(ctx, guard, e);
    let missing = |name: &str| Fault::Failed(format!("the plugin exports no `{name}`"));
    match request {
        Request::Context {
            about,
            language,
            hotwords,
        } => {
            let function = hooks.context.clone().ok_or_else(|| missing("context"))?;
            let function = function.restore(ctx).map_err(fail)?;
            let arg = about_object(ctx, &about).map_err(fail)?;
            arg.set("language", language).map_err(fail)?;
            arg.set("hotwords", hotwords).map_err(fail)?;
            let returned: Value = function.call((arg,)).map_err(fail)?;
            let settled = settle(ctx, guard, returned)?;
            extra(ctx, guard, &settled).map(Answer::Extra)
        }
        Request::Transform { about, text } => {
            let function = hooks
                .transform
                .clone()
                .ok_or_else(|| missing("transform"))?;
            let function = function.restore(ctx).map_err(fail)?;
            let arg = about_object(ctx, &about).map_err(fail)?;
            let returned: Value = function.call((text, arg)).map_err(fail)?;
            let settled = settle(ctx, guard, returned)?;
            match settled.as_string() {
                Some(s) => s.to_string().map(Answer::Text).map_err(fail),
                None => Err(Fault::Invalid(format!(
                    "transform returned {}, not a string",
                    type_name(&settled)
                ))),
            }
        }
    }
}

fn about_object<'js>(ctx: &Ctx<'js>, about: &About) -> rquickjs::Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    if let Some(session) = &about.session {
        object.set("session", session.as_str())?;
    }
    object.set("source", about.source)?;
    object.set("app", about.app.as_deref())?;
    let settings = Object::new(ctx.clone())?;
    for (key, value) in &about.settings {
        settings.set(key.as_str(), value.as_str())?;
    }
    object.set("settings", settings)?;
    Ok(object)
}

/// A hook's answer once any promise it returned has settled. A promise that
/// cannot settle within the call's deadline, or by running what is queued,
/// is a timeout.
fn settle<'js>(ctx: &Ctx<'js>, guard: &Guard, value: Value<'js>) -> Result<Value<'js>, Fault> {
    let Some(promise) = value.as_promise() else {
        return Ok(value);
    };
    loop {
        match promise.result::<Value>() {
            Some(Ok(settled)) => return Ok(settled),
            Some(Err(e)) => return Err(fault(ctx, guard, e)),
            None => {}
        }
        if guard.expired() || guard.stopped() || !ctx.execute_pending_job() {
            return Err(Fault::Timeout);
        }
    }
}

/// `undefined`, `null` or `{ hotwords?: string[], language?: string }`.
fn extra<'js>(ctx: &Ctx<'js>, guard: &Guard, value: &Value<'js>) -> Result<Extra, Fault> {
    let fail = |e| fault(ctx, guard, e);
    let wrong = |what: String| Fault::Invalid(format!("context returned {what}"));
    if value.is_undefined() || value.is_null() {
        return Ok(Extra::default());
    }
    let object = match value.as_object() {
        Some(object) if !value.is_array() => object,
        _ => return Err(wrong(format!("{}, not an object", type_name(value)))),
    };
    let mut out = Extra::default();
    for key in object.keys::<String>() {
        let key = key.map_err(fail)?;
        let field: Value = object.get(key.as_str()).map_err(fail)?;
        match key.as_str() {
            "hotwords" if field.is_undefined() || field.is_null() => {}
            "hotwords" => {
                let Some(list) = field.as_array() else {
                    return Err(wrong(format!(
                        "hotwords as {}, not an array",
                        type_name(&field)
                    )));
                };
                for i in 0..list.len() {
                    let word: Value = list.get(i).map_err(fail)?;
                    let Some(word) = word.as_string() else {
                        return Err(wrong("hotwords that are not all strings".to_owned()));
                    };
                    out.hotwords.push(word.to_string().map_err(fail)?);
                }
            }
            "language" if field.is_undefined() || field.is_null() => {}
            "language" => {
                let Some(language) = field.as_string() else {
                    return Err(wrong(format!(
                        "a language of {}, not a string",
                        type_name(&field)
                    )));
                };
                let language = language.to_string().map_err(fail)?;
                out.language = (!language.is_empty()).then_some(language);
            }
            other => return Err(wrong(format!("an unknown field `{other}`"))),
        }
    }
    Ok(out)
}

fn type_name(value: &Value<'_>) -> String {
    format!("{:?}", value.type_of()).to_lowercase()
}
