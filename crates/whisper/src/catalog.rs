//! The models Anya offers to download: whisper.cpp's multilingual ggml
//! models, each pinned to the SHA-1 whisper.cpp publishes for it
//! (`models/README.md` in whisper.cpp). English-only models are left out:
//! one model serves every language Anya offers.

use std::sync::LazyLock;

use anya_protocol::Text;

/// One downloadable model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// What the config's `model` setting names it by.
    pub id: &'static str,
    /// The file name, at the download source and on disk.
    pub file: &'static str,
    /// About how large the file is, for the list; progress uses the server's length.
    pub bytes: u64,
    /// The file's SHA-1, lowercase hex.
    pub sha1: &'static str,
    /// One line on what it is good for: English, then Simplified Chinese.
    pub(crate) note: [&'static str; 2],
}

impl Model {
    /// The one line on what it is good for.
    pub fn note(&self) -> Text {
        Text::localized([("en", self.note[0]), ("zh-CN", self.note[1])])
    }
}

const MIB: u64 = 1 << 20;

/// Where the files are fetched from unless the user names a mirror.
pub const DEFAULT_SOURCE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// The models, smallest first.
pub const MODELS: &[Model] = &[
    Model {
        id: "base",
        file: "ggml-base.bin",
        bytes: 142 * MIB,
        sha1: "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
        note: [
            "Smallest and fastest; often mishears Chinese",
            "最小最快，中文容易听错",
        ],
    },
    Model {
        id: "small",
        file: "ggml-small.bin",
        bytes: 466 * MIB,
        sha1: "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
        note: [
            "Uses less memory, a little less accurate",
            "更省内存，准确率稍低",
        ],
    },
    Model {
        id: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        bytes: 547 * MIB,
        sha1: "e050f7970618a659205450ad97eb95a18d69c9ee",
        note: [
            "Accurate and fast; suits most computers",
            "准确又快，适合大多数电脑",
        ],
    },
    Model {
        id: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        bytes: 1536 * MIB,
        sha1: "4af2b29d7ec73d781377bfd1758ca957a807e941",
        note: [
            "Slightly more accurate than the compressed one, about three times the memory",
            "比压缩版略准，内存约三倍",
        ],
    },
    Model {
        id: "large-v3",
        file: "ggml-large-v3.bin",
        bytes: 2970 * MIB,
        sha1: "ad82bf6a9043ceed055076d0fd39f5f186ff8062",
        note: ["Most accurate and slowest", "最准，最慢"],
    },
];

/// Below this much memory the recommended model is `small`.
const SMALL_BELOW: u64 = 8 << 30;

/// The model to suggest for a computer with `memory` bytes of RAM, when known.
pub(crate) fn recommended(memory: Option<u64>) -> &'static str {
    match memory {
        Some(bytes) if bytes < SMALL_BELOW => "small",
        _ => "large-v3-turbo-q5_0",
    }
}

/// This computer's RAM, read once.
pub(crate) static MEMORY: LazyLock<Option<u64>> = LazyLock::new(memory);

/// This computer's RAM: `hw.memsize` on macOS, `MemTotal` on Linux.
fn memory() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    } else {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib = info
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .ok()?;
        Some(kib * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_computer_is_offered_the_small_model() {
        assert_eq!(recommended(Some(4 << 30)), "small");
        assert_eq!(recommended(Some(16 << 30)), "large-v3-turbo-q5_0");
        assert_eq!(recommended(None), "large-v3-turbo-q5_0");
    }

    #[test]
    fn every_model_is_pinned_and_named_once() {
        for (i, m) in MODELS.iter().enumerate() {
            assert_eq!(m.file, format!("ggml-{}.bin", m.id));
            assert_eq!(m.sha1.len(), 40);
            assert!(
                m.sha1
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert!(MODELS[..i].iter().all(|o| o.id != m.id));
            assert!(!m.note().get("zh-CN").is_empty() && !m.note().get("en").is_empty());
        }
        for memory in [None, Some(1)] {
            assert!(MODELS.iter().any(|m| m.id == recommended(memory)));
        }
    }
}
