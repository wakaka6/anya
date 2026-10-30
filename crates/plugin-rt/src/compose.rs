use std::collections::BTreeMap;

/// Why a plugin is on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The user has said nothing about it: plugins are on.
    Default,
    /// The user's row decides.
    User,
    /// The user's row says off, but what the user has made with the plugin
    /// needs it, so it stays on.
    InUse,
}

/// One plugin's composed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// The plugin id.
    pub id: String,
    /// Whether it is wanted running.
    pub on: bool,
    /// Which layer decided.
    pub why: Why,
}

/// What is wanted of each plugin in `ids`, in their order: on unless a row of
/// `rows` (plugin id to the user's choice) says off, and on whatever the row
/// says while `in_use` holds for it.
pub fn compose<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    rows: &BTreeMap<String, bool>,
    in_use: impl Fn(&str) -> bool,
) -> Vec<Composed> {
    ids.into_iter()
        .map(|id| {
            let (on, why) = match rows.get(id) {
                None => (true, Why::Default),
                Some(true) => (true, Why::User),
                Some(false) if in_use(id) => (true, Why::InUse),
                Some(false) => (false, Why::User),
            };
            Composed {
                id: id.to_owned(),
                on,
                why,
            }
        })
        .collect()
}
