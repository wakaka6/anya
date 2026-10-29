use serde::{Deserialize, Serialize};

use crate::Text;

/// How an engine describes itself. Built-in engines return one from code;
/// engine plugins send the same JSON when they load. Settings UIs draw an
/// engine's settings from [`Manifest::fields`], so no UI names an engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable id and config key: lowercase ASCII letters, digits and `-`.
    pub id: String,
    /// Name to show.
    pub title: Text,
    /// One line under the title.
    #[serde(default, skip_serializing_if = "Text::is_empty")]
    pub blurb: Text,
    /// Where the engine's own documentation lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// Results arrive while the user speaks; otherwise one result after the audio ends.
    pub streaming: bool,
    /// The engine's settings, in the order to show them.
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// One engine setting, stored under the engine's id in the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// Key in the engine's settings map.
    pub key: String,
    /// Name to show.
    pub label: Text,
    /// One line of help.
    #[serde(default, skip_serializing_if = "Text::is_empty")]
    pub help: Text,
    /// What the value is.
    #[serde(flatten)]
    pub kind: FieldKind,
    /// The engine cannot open a session without it.
    #[serde(default)]
    pub required: bool,
    /// Has a working default; UIs may fold it away.
    #[serde(default)]
    pub advanced: bool,
}

/// What a [`Field`] holds, as its `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    /// Free text.
    Text,
    /// A credential: masked in UIs, never logged.
    Secret,
    /// One of `options`.
    Choice {
        /// The allowed values, in the order to show them.
        options: Vec<String>,
    },
}
