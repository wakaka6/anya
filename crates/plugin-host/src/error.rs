/// Why a plugin operation failed. The Display text is English, for logs and
/// for showing beside a translated message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginError {
    /// A download or a read of `file://` failed.
    #[error("cannot fetch {url}: {reason}")]
    Fetch {
        /// What was asked for.
        url: String,
        /// The network's or the server's own words.
        reason: String,
    },
    /// The registry index is not valid JSON of the expected shape.
    #[error("the registry index is invalid: {0}")]
    Index(String),
    /// The registry index is a format version this Anya does not read.
    #[error("the registry index has format version {found}, this Anya reads {supported}")]
    IndexVersion {
        /// The index's `version`.
        found: u32,
        /// The version Anya reads.
        supported: u32,
    },
    /// The index has no such plugin release.
    #[error("the registry has no plugin {id} version {version}")]
    NotInIndex {
        /// The plugin id asked for.
        id: String,
        /// The version asked for.
        version: String,
    },
    /// The release cannot run here: another protocol, another Anya, another kind.
    #[error("plugin {id} {version} cannot run here: {reason}")]
    Incompatible {
        /// The plugin id.
        id: String,
        /// The release.
        version: String,
        /// What does not fit.
        reason: String,
    },
    /// The downloaded bytes do not match the hash the index promised.
    #[error("integrity check failed for {what}: expected {expected}, got {actual}")]
    Integrity {
        /// The artifact checked.
        what: String,
        /// The hash the index or the pinned table names.
        expected: String,
        /// The hash of the bytes received.
        actual: String,
    },
    /// An integrity string in a format other than `sha512-<base64>`.
    #[error("unsupported integrity string {0:?}; expected sha512-<base64>")]
    IntegrityFormat(String),
    /// An id that is not lowercase ASCII letters, digits and `-`, or a
    /// version that is not semver.
    #[error("invalid {what} {value:?}")]
    Invalid {
        /// `plugin id` or `version`.
        what: &'static str,
        /// The offending text.
        value: String,
    },
    /// No installed plugin has this id.
    #[error("plugin {0} is not installed")]
    NotInstalled(String),
    /// The tarball or zip cannot be unpacked safely.
    #[error("cannot unpack {what}: {reason}")]
    Archive {
        /// The artifact.
        what: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Reading or writing a file failed.
    #[error("{path}: {reason}")]
    Io {
        /// The file or directory.
        path: String,
        /// The system's words.
        reason: String,
    },
    /// No Bun is installed and none can be downloaded for this platform.
    #[error("no Bun runtime for this platform: {0}")]
    Bun(String),
}

impl PluginError {
    /// An [`PluginError::Io`] for `path`.
    pub(crate) fn io(path: &std::path::Path, e: &std::io::Error) -> PluginError {
        PluginError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        }
    }
}
