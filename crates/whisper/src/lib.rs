//! whisper.cpp on this computer, run by Anya (docs/decisions/0018): the
//! models it offers and downloads, and the `whisper-server` it starts for a
//! session and stops when idle. It names no engine: the app hands it the
//! whisper.cpp engine to wrap with [`Local::wrap`].

mod catalog;
mod managed;
mod models;
mod server;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anya_core::engine::Engine;

pub use catalog::{DEFAULT_SOURCE, MODELS, Model};
pub use managed::{EXTERNAL, IDLE, MANAGED, MODE, MODEL, PROGRAM, SOURCE};
pub use models::{DownloadError, ModelState, ModelStatus, UnknownModel};
pub use server::{Launch, ServerState, StartError, find_program};

use models::Models;
use server::Server;

/// The models directory and the one server Anya runs.
pub struct Local {
    models: Models,
    server: Arc<Server>,
}

/// Everything the settings show about the managed server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Every catalog model and where it is.
    pub models: Vec<ModelStatus>,
    /// The model to suggest for this computer.
    pub recommended: &'static str,
    /// Where models are kept.
    pub dir: PathBuf,
    /// Bytes the models there take, partial downloads included.
    pub used: u64,
    /// The whisper-server the settings would start, if there is one.
    pub program: Option<PathBuf>,
    /// The server now.
    pub server: ServerState,
}

impl Local {
    /// Keeps models in `dir`.
    pub fn new(dir: PathBuf) -> Arc<Local> {
        Local::with_catalog(dir, catalog::MODELS)
    }

    pub(crate) fn with_catalog(dir: PathBuf, catalog: &'static [Model]) -> Arc<Local> {
        Arc::new(Local {
            models: Models::new(dir, catalog),
            server: Arc::default(),
        })
    }

    /// `engine`, a whisper.cpp engine that reads the server's address from
    /// its `url` setting, with the settings and sessions of a managed server.
    pub fn wrap(self: &Arc<Self>, engine: Arc<dyn Engine>) -> Arc<dyn Engine> {
        Arc::new(managed::Managed::new(engine, Arc::clone(self)))
    }

    /// Where models are kept.
    pub fn dir(&self) -> &Path {
        self.models.dir()
    }

    /// The models, the program `settings` name and the server, now.
    pub fn status(&self, settings: &BTreeMap<String, String>) -> Status {
        Status {
            models: self.models.list(),
            recommended: catalog::recommended(*catalog::MEMORY),
            dir: self.models.dir().to_owned(),
            used: self.models.used(),
            program: find_program(settings.get(PROGRAM).map_or("", String::as_str)),
            server: self.server.state(),
        }
    }

    /// Starts downloading model `id` from the source `settings` name, on the
    /// current Tokio runtime, and returns at once. A model that is already
    /// downloaded or downloading is left alone.
    pub fn download(
        &self,
        id: &str,
        settings: &BTreeMap<String, String>,
    ) -> Result<(), UnknownModel> {
        self.models
            .download(id, settings.get(SOURCE).map_or("", String::as_str))
    }

    /// Stops downloading model `id`; the next download resumes from what is on disk.
    pub fn cancel(&self, id: &str) {
        self.models.cancel(id);
    }

    /// Deletes model `id`, stopping the server first if it runs it.
    pub async fn delete(&self, id: &str) -> std::io::Result<()> {
        if let Some(path) = self.models.ready(id)
            && matches!(self.server.state(), ServerState::Starting { model } | ServerState::Running { model, .. } if model == path)
        {
            self.server.stop().await;
        }
        self.models.delete(id)
    }

    /// Stops the server if it runs; the next session that needs it starts it again.
    pub async fn stop(&self) {
        self.server.stop().await;
    }
}

#[cfg(test)]
mod tests;
