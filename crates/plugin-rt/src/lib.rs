//! The plugin runtime (docs/decisions/0012-built-in-plugin-runtime.md).
//!
//! A [`Plugin`] serves things into [`Point`]s: places the app defines, such as
//! the list of engines. What it serves is held by [`Registration`]s that the
//! plugin gathers into an [`Active`]; dropping the `Active` withdraws it all.
//! [`compose`] decides which plugins the user wants on and why, and
//! [`Runtime::reconcile`] brings the running set to that.
//!
//! This crate knows no seam of Anya's. The app picks the type `P` its plugins
//! serve into and builds the list of plugins; nothing registers itself.

mod compose;
mod point;
mod runtime;

pub use compose::{Composed, Why, compose};
pub use point::{Point, Registration};
pub use runtime::{Active, ApplyError, Error, Plugin, Runtime, State};
