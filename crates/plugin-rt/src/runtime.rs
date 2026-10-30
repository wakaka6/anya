use std::collections::BTreeMap;

/// Why a plugin could not start. The text is English, for logs and for
/// showing beside a translated message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ApplyError(pub String);

/// Why a [`Runtime`] cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Two plugins have one id.
    #[error("two plugins are called {0:?}")]
    DuplicateId(String),
    /// An id that is not lowercase ASCII letters, digits and `-`.
    #[error("{0:?} is not a plugin id: use lowercase letters, digits and '-'")]
    BadId(String),
}

/// What a plugin holds while it runs: the [`crate::Registration`]s it made, or
/// anything else that must go when the plugin does. Dropping it releases the
/// newest first.
#[derive(Default)]
pub struct Active {
    held: Vec<Box<dyn Send + Sync>>,
}

impl Active {
    /// Holds nothing yet.
    pub fn new() -> Active {
        Active::default()
    }

    /// Also holds `guard`, released with the rest.
    #[must_use]
    pub fn hold(mut self, guard: impl Send + Sync + 'static) -> Active {
        self.held.push(Box::new(guard));
        self
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        while let Some(guard) = self.held.pop() {
            drop(guard);
        }
    }
}

/// A piece of Anya the user can switch off. `P` is what plugins serve into,
/// defined by the app.
pub trait Plugin<P>: Send + Sync {
    /// Lowercase ASCII letters, digits and `-`; stable, since users' choices
    /// are kept under it.
    fn id(&self) -> &str;

    /// Starts serving into `points`. What it serves lasts as long as the
    /// returned [`Active`].
    fn apply(&self, points: &P) -> Result<Active, ApplyError>;
}

/// Where a plugin is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not wanted; nothing of it runs.
    Off,
    /// Wanted and running.
    Active,
    /// Wanted, but [`Plugin::apply`] failed; carries the reason.
    Failed(String),
}

/// Plugins and what they serve into. One plugin failing never stops another.
pub struct Runtime<P> {
    points: P,
    plugins: Vec<Box<dyn Plugin<P>>>,
    /// Running plugins (index into `plugins`) in the order they started.
    active: Vec<(usize, Active)>,
    failed: BTreeMap<usize, String>,
}

impl<P> Runtime<P> {
    /// A runtime over `plugins`, all off, serving into `points`.
    pub fn new(points: P, plugins: Vec<Box<dyn Plugin<P>>>) -> Result<Runtime<P>, Error> {
        let mut seen = Vec::new();
        for plugin in &plugins {
            let id = plugin.id();
            let valid = !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !valid {
                return Err(Error::BadId(id.to_owned()));
            }
            if seen.contains(&id) {
                return Err(Error::DuplicateId(id.to_owned()));
            }
            seen.push(id);
        }
        Ok(Runtime {
            points,
            plugins,
            active: Vec::new(),
            failed: BTreeMap::new(),
        })
    }

    /// What plugins serve into.
    pub fn points(&self) -> &P {
        &self.points
    }

    /// The plugin ids, in order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(|p| p.id())
    }

    /// Brings the running plugins to those `on` says yes to: the ones no
    /// longer wanted are released newest first, then the wanted ones that are
    /// not running start, in list order. A plugin that failed before is tried
    /// again.
    pub fn reconcile(&mut self, on: impl Fn(&str) -> bool) {
        let mut at = self.active.len();
        while at > 0 {
            at -= 1;
            if !on(self.plugins[self.active[at].0].id()) {
                self.active.remove(at);
            }
        }
        self.failed.clear();
        for (index, plugin) in self.plugins.iter().enumerate() {
            if !on(plugin.id()) || self.active.iter().any(|(running, _)| *running == index) {
                continue;
            }
            match plugin.apply(&self.points) {
                Ok(held) => self.active.push((index, held)),
                Err(e) => {
                    tracing::warn!(plugin = plugin.id(), "plugin did not start: {e}");
                    self.failed.insert(index, e.0);
                }
            }
        }
    }

    /// Every plugin and where it is, in list order.
    pub fn states(&self) -> Vec<(&str, State)> {
        self.plugins
            .iter()
            .enumerate()
            .map(|(index, plugin)| {
                let state = if let Some(reason) = self.failed.get(&index) {
                    State::Failed(reason.clone())
                } else if self.active.iter().any(|(running, _)| *running == index) {
                    State::Active
                } else {
                    State::Off
                };
                (plugin.id(), state)
            })
            .collect()
    }
}

impl<P> Drop for Runtime<P> {
    fn drop(&mut self) {
        // The list drops oldest first; plugins go newest first.
        while self.active.pop().is_some() {}
    }
}
