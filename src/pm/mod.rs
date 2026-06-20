//! coven — witchy's package manager.
//!
//! A rune is a package; coven is the registry. The whole design is in
//! `rfcs/package-manager.md`. The guiding invariant: this tool only moves bytes
//! and verifies them — it never becomes a source of ambient authority. Its jobs
//! are integrity, reproducibility, and making each rune's capability footprint
//! (runtime *and* build) legible and gated.

pub mod cli;
pub mod footprint;
pub mod gate;
pub mod http;
pub mod keys;
pub mod lockfile;
pub mod manifest;
pub mod registry;
pub mod remote;
pub mod resolve;
pub mod semver;
pub mod server;
pub mod store;
pub mod trusted;
pub mod tuf;
pub mod wire;

use std::fmt;

/// Any error surfaced by a coven subcommand.
#[derive(Debug)]
pub struct PmError(pub String);

impl fmt::Display for PmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PmError {}

impl From<std::io::Error> for PmError {
    fn from(e: std::io::Error) -> Self {
        PmError(e.to_string())
    }
}

pub type PmResult<T> = Result<T, PmError>;

pub fn err<T>(msg: impl Into<String>) -> PmResult<T> {
    Err(PmError(msg.into()))
}

/// The current directory's project entry source file (`src/<module>.witchy`
/// per the manifest's rune name), if we're inside a project at all. Lets
/// file-oriented commands (`witchy caps`) default to the project entry.
pub fn project_entry_file() -> Option<String> {
    let dir = std::path::Path::new(".");
    let manifest = manifest::Manifest::load(dir).ok()?;
    let module = manifest.rune.name.rsplit('/').next().unwrap_or("").replace('-', "_");
    let path = dir.join("src").join(format!("{module}.witchy"));
    path.exists().then(|| path.display().to_string())
}
