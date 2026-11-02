//! An unpacked plugin package: its `package.json` and the `anya` field in it.

use std::path::{Component, Path, PathBuf};

use anya_protocol::{PLUGIN_PROTOCOL, PluginPackage, Text};
use serde_json::Value;

use crate::PluginError;

/// An unpacked plugin package, as its `package.json` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// `anya.id`, else the package name.
    pub id: String,
    /// Where the package is unpacked.
    pub dir: PathBuf,
    /// `anya.name`, else the package name.
    pub name: Text,
    /// `anya.description`, else the package description; empty when neither.
    pub description: Text,
    /// The `anya` field.
    pub anya: PluginPackage,
}

/// Reads the `package.json` in `dir`. A package without an `anya` field, or
/// with one that does not parse, names no entry, names one outside `dir` or
/// missing, or was built for another plugin protocol, is an error naming
/// the file.
pub fn read_package(dir: &Path) -> Result<Package, PluginError> {
    let path = dir.join("package.json");
    let invalid = |reason: String| PluginError::Io {
        path: path.display().to_string(),
        reason,
    };
    let bytes = std::fs::read(&path).map_err(|e| PluginError::io(&path, &e))?;
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("it is not valid JSON ({e})")))?;
    let anya = json
        .get("anya")
        .ok_or_else(|| invalid("it has no \"anya\" field".to_owned()))?;
    if anya.get("engine").is_some() {
        return Err(invalid("\"anya.engine\" is now \"anya.host\"".to_owned()));
    }
    let anya: PluginPackage = serde_json::from_value(anya.clone())
        .map_err(|e| invalid(format!("its \"anya\" field is invalid ({e})")))?;
    if anya.inline.is_none() && anya.host.is_none() {
        return Err(invalid(
            "it names neither \"anya.inline\" nor \"anya.host\"".to_owned(),
        ));
    }
    if let Some(protocol) = anya.protocol.filter(|p| *p != PLUGIN_PROTOCOL) {
        return Err(invalid(format!(
            "it was built for plugin protocol {protocol}, this Anya speaks {PLUGIN_PROTOCOL}"
        )));
    }
    for (key, entry) in [("inline", &anya.inline), ("host", &anya.host)] {
        let Some(entry) = entry else { continue };
        let inside = Path::new(entry)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !inside || !dir.join(entry).is_file() {
            return Err(invalid(format!(
                "\"anya.{key}\" ({entry}) is not a file in the package"
            )));
        }
    }
    let text = |key: &str| json.get(key).and_then(Value::as_str).map(str::to_owned);
    let package_name = text("name");
    let id = anya
        .id
        .clone()
        .or_else(|| package_name.clone())
        .ok_or_else(|| invalid("it has neither \"anya.id\" nor \"name\"".to_owned()))?;
    let name = anya
        .name
        .clone()
        .unwrap_or_else(|| Text::Plain(package_name.unwrap_or_else(|| id.clone())));
    let description = anya
        .description
        .clone()
        .unwrap_or_else(|| Text::Plain(text("description").unwrap_or_default()));
    Ok(Package {
        id,
        dir: dir.to_owned(),
        name,
        description,
        anya,
    })
}
