//! The models on disk and their downloads. A download goes to
//! `<file>.part`, resumes from its length, and becomes `<file>` only after
//! its SHA-1 matches the catalog's: a file under its own name is whole and
//! the model the catalog names, whatever the source.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use reqwest::{Client, StatusCode, Url, header};
use ring::digest;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use crate::catalog::{self, Model};

/// How long connecting to the source may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the source may send nothing before the download fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a download failed. The Display text is English, for logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    /// The source address is not an `http` or `https` URL.
    #[error("the download source {0:?} is not an http or https address")]
    InvalidSource(String),
    /// The source could not be reached, or the connection broke.
    #[error("cannot reach {host}: {detail}")]
    Connect {
        /// The source's host.
        host: String,
        /// What the network said.
        detail: String,
    },
    /// The source answered with an error status.
    #[error("the source answered HTTP {0}")]
    Http(u16),
    /// No room left on the disk.
    #[error("the disk is full")]
    DiskFull,
    /// Writing the file failed for another reason.
    #[error("cannot write the model: {0}")]
    Write(String),
    /// The whole file arrived but is not the model the catalog pins; it was deleted.
    #[error("the downloaded file does not match its pinned SHA-1")]
    Mismatch,
}

/// A model id the catalog does not have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no model is called {0:?}")]
pub struct UnknownModel(pub String);

/// Where one model is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// Nothing on disk.
    Absent,
    /// Part of it is on disk, no download running: the next one resumes.
    Partial {
        /// Bytes on disk.
        have: u64,
    },
    /// Downloading now.
    Downloading {
        /// Bytes on disk.
        have: u64,
        /// The whole file, once the source said.
        total: Option<u64>,
    },
    /// Whole; its hash is being checked.
    Verifying,
    /// On disk, whole and checked.
    Ready {
        /// Its size.
        bytes: u64,
    },
    /// The last download failed; what it left on disk resumes the next.
    Failed {
        /// Why.
        error: DownloadError,
        /// Bytes on disk.
        have: u64,
    },
}

/// A catalog model and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStatus {
    /// The model.
    pub model: Model,
    /// Where it is.
    pub state: ModelState,
}

/// The models directory and the downloads into it.
pub(crate) struct Models {
    shared: Arc<Shared>,
}

struct Shared {
    dir: PathBuf,
    catalog: &'static [Model],
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    running: HashMap<&'static str, Job>,
    /// Cancelled downloads that may not have stopped writing yet.
    stopping: HashMap<&'static str, JoinHandle<()>>,
    failed: HashMap<&'static str, DownloadError>,
    next: u64,
}

struct Job {
    generation: u64,
    progress: Arc<Progress>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct Progress {
    have: AtomicU64,
    /// Zero until the source says.
    total: AtomicU64,
    verifying: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

impl Models {
    pub(crate) fn new(dir: PathBuf, catalog: &'static [Model]) -> Models {
        Models {
            shared: Arc::new(Shared {
                dir,
                catalog,
                jobs: Mutex::default(),
            }),
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.shared.dir
    }

    pub(crate) fn find(&self, id: &str) -> Option<&'static Model> {
        self.shared.catalog.iter().find(|m| m.id == id)
    }

    /// Where model `id` is once downloaded, if it is now.
    pub(crate) fn ready(&self, id: &str) -> Option<PathBuf> {
        let model = self.find(id)?;
        let path = self.shared.dir.join(model.file);
        path.is_file().then_some(path)
    }

    /// Every catalog model and where it is.
    pub(crate) fn list(&self) -> Vec<ModelStatus> {
        let jobs = lock(&self.shared.jobs);
        self.shared
            .catalog
            .iter()
            .map(|model| {
                let path = self.shared.dir.join(model.file);
                let have = len(&part(&path));
                let state = if let Some(job) = jobs.running.get(model.id) {
                    if job.progress.verifying.load(Ordering::Relaxed) {
                        ModelState::Verifying
                    } else {
                        ModelState::Downloading {
                            have: job.progress.have.load(Ordering::Relaxed),
                            total: Some(job.progress.total.load(Ordering::Relaxed))
                                .filter(|&t| t > 0),
                        }
                    }
                } else if path.is_file() {
                    ModelState::Ready { bytes: len(&path) }
                } else if let Some(error) = jobs.failed.get(model.id) {
                    ModelState::Failed {
                        error: error.clone(),
                        have,
                    }
                } else if have > 0 {
                    ModelState::Partial { have }
                } else {
                    ModelState::Absent
                };
                ModelStatus {
                    model: *model,
                    state,
                }
            })
            .collect()
    }

    /// Bytes the downloaded models and partial downloads take.
    pub(crate) fn used(&self) -> u64 {
        self.shared
            .catalog
            .iter()
            .map(|m| {
                let path = self.shared.dir.join(m.file);
                len(&path) + len(&part(&path))
            })
            .sum()
    }

    /// Starts downloading model `id` from `source` (a base URL; empty is
    /// the default) on the current Tokio runtime and returns at once.
    /// Nothing happens for a model that is downloaded or downloading.
    pub(crate) fn download(&self, id: &str, source: &str) -> Result<(), UnknownModel> {
        let model = *self.find(id).ok_or_else(|| UnknownModel(id.to_owned()))?;
        let mut jobs = lock(&self.shared.jobs);
        if jobs.running.contains_key(model.id) || self.ready(model.id).is_some() {
            return Ok(());
        }
        jobs.failed.remove(model.id);
        let previous = jobs.stopping.remove(model.id);
        jobs.next += 1;
        let generation = jobs.next;
        let progress = Arc::new(Progress::default());
        let source = match source.trim().trim_end_matches('/') {
            "" => catalog::DEFAULT_SOURCE.to_owned(),
            s => s.to_owned(),
        };
        let shared = Arc::clone(&self.shared);
        let task = tokio::spawn({
            let progress = Arc::clone(&progress);
            async move {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                let outcome = fetch(&shared.dir, &model, &source, &progress).await;
                let mut jobs = lock(&shared.jobs);
                if jobs
                    .running
                    .get(model.id)
                    .is_some_and(|j| j.generation == generation)
                {
                    jobs.running.remove(model.id);
                    match outcome {
                        Ok(()) => tracing::info!(model = model.id, "model downloaded"),
                        Err(error) => {
                            tracing::warn!(model = model.id, %error, "model download failed");
                            jobs.failed.insert(model.id, error);
                        }
                    }
                }
            }
        });
        jobs.running.insert(
            model.id,
            Job {
                generation,
                progress,
                task,
            },
        );
        Ok(())
    }

    /// Stops downloading model `id`; what is on disk resumes the next download.
    pub(crate) fn cancel(&self, id: &str) {
        let mut jobs = lock(&self.shared.jobs);
        if let Some((key, job)) = jobs.running.remove_entry(id) {
            job.task.abort();
            jobs.stopping.insert(key, job.task);
        }
    }

    /// Deletes model `id` and any partial download of it.
    pub(crate) fn delete(&self, id: &str) -> std::io::Result<()> {
        let Some(model) = self.find(id) else {
            return Ok(());
        };
        self.cancel(id);
        lock(&self.shared.jobs).failed.remove(model.id);
        let path = self.shared.dir.join(model.file);
        for file in [part(&path), path] {
            match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

fn part(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    path.with_file_name(name)
}

fn write_error(e: &std::io::Error) -> DownloadError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        DownloadError::DiskFull
    } else {
        DownloadError::Write(e.to_string())
    }
}

async fn fetch(
    dir: &Path,
    model: &Model,
    source: &str,
    progress: &Progress,
) -> Result<(), DownloadError> {
    let url = Url::parse(&format!("{source}/{}", model.file))
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| DownloadError::InvalidSource(source.to_owned()))?;
    let host = url.host_str().unwrap_or_default().to_owned();
    let network = |e: reqwest::Error| {
        let e = e.without_url();
        let mut detail = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(cause) = source {
            detail.push_str(": ");
            detail.push_str(&cause.to_string());
            source = cause.source();
        }
        DownloadError::Connect {
            host: host.clone(),
            detail,
        }
    };
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| write_error(&e))?;
    let path = dir.join(model.file);
    let part = part(&path);
    let mut have = len(&part);
    progress.have.store(have, Ordering::Relaxed);

    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent("anya")
        .build()
        .map_err(&network)?;
    let mut request = client.get(url.clone());
    if have > 0 {
        request = request.header(header::RANGE, format!("bytes={have}-"));
    }
    tracing::info!(model = model.id, %url, from = have, "downloading model");
    let mut response = request.send().await.map_err(&network)?;
    let status = response.status();
    let resumed = match status {
        StatusCode::PARTIAL_CONTENT => true,
        StatusCode::OK => false,
        // The part is already whole: nothing is left to send.
        StatusCode::RANGE_NOT_SATISFIABLE if have > 0 => true,
        other => return Err(DownloadError::Http(other.as_u16())),
    };
    if status != StatusCode::RANGE_NOT_SATISFIABLE {
        if !resumed {
            have = 0;
        }
        if let Some(rest) = response.content_length() {
            progress.total.store(have + rest, Ordering::Relaxed);
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .await
            .map_err(|e| write_error(&e))?;
        while let Some(chunk) = response.chunk().await.map_err(&network)? {
            file.write_all(&chunk).await.map_err(|e| write_error(&e))?;
            have += chunk.len() as u64;
            progress.have.store(have, Ordering::Relaxed);
        }
        file.flush().await.map_err(|e| write_error(&e))?;
        file.sync_all().await.map_err(|e| write_error(&e))?;
    }

    progress.verifying.store(true, Ordering::Relaxed);
    let checked = part.clone();
    let digest = tokio::task::spawn_blocking(move || sha1(&checked))
        .await
        .map_err(|e| DownloadError::Write(e.to_string()))?
        .map_err(|e| write_error(&e))?;
    if digest != model.sha1 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(DownloadError::Mismatch);
    }
    tokio::fs::rename(&part, &path)
        .await
        .map_err(|e| write_error(&e))
}

/// The SHA-1 of the file at `path`, lowercase hex.
fn sha1(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut context = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        context.update(&buf[..n]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
