//! Backup engine abstraction.
//!
//! Two implementations:
//!   - [`rustic::RusticEngine`] — deduplicated, encrypted, restic-compatible
//!     storage. Best fit for dev-repos, log files, anything where dedup
//!     and immutable snapshot history have measurable value.
//!   - [`mirror::MirrorEngine`] — versioned mirror: source-shaped
//!     destination plus `.versions/` for overwritten / deleted files.
//!     Best fit for photos, documents, anything the user wants to
//!     browse straight from Explorer.
//!
//! Callers obtain an engine via [`engine_for`], which dispatches on the
//! repository's `backend:` field declared in `kovre.yaml`.

pub mod mirror;
pub mod rustic;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::{BackendKind, Repository as RepoConfig, Retention};

pub use mirror::MirrorEngine;
pub use rustic::RusticEngine;

/// Tag prefix kovre attaches to every rustic snapshot so we can later
/// filter by job. Mirror does not produce snapshots in the rustic sense
/// — its history lives under `.versions/` on disk.
pub const JOB_TAG_PREFIX: &str = "kovre-job:";

/// What kovre wants to back up: a list of source paths plus exclude
/// globs. Excludes follow restic semantics in the YAML (bare pattern =
/// "exclude this"); engines translate to their own internal form.
#[derive(Debug, Clone)]
pub struct BackupSource {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

/// Domain-level summary of a snapshot — independent of rustic types so
/// the CLI (and dashboard) doesn't have to import `rustic_core`.
/// Engines without a native snapshot concept (mirror) synthesize one
/// per backup run from the run's wall-clock timestamp.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub id: String,
    pub time: String,
    pub paths: Vec<String>,
    pub hostname: String,
    pub tags: Vec<String>,
    pub total_bytes_processed: Option<u64>,
    pub data_added: Option<u64>,
}

/// Outcome of applying retention to a single job's history.
#[derive(Debug, Clone, Default)]
pub struct RetentionOutcome {
    pub kept: usize,
    pub forgotten: usize,
}

/// Backup engine — the abstraction every backend implements.
///
/// Implementations should be cheap to construct (they only hold a
/// reference to the `RepoConfig`) but may do real I/O on the lifecycle
/// methods.
pub trait BackupEngine: Send + Sync {
    /// Materialize the repository on disk. For rustic, runs `init`.
    /// For mirror, creates the destination directory. Already-
    /// initialized repos return an error the caller can match on
    /// (rustic surfaces "config file already exists"); the dashboard
    /// translates that to 409 `already_initialized`.
    fn init(&self) -> Result<()>;

    /// Run a backup for `job_name` against `source`.
    fn backup(&self, job_name: &str, source: BackupSource) -> Result<SnapshotInfo>;

    /// Enumerate the snapshots known for this job. Mirror returns an
    /// empty vec — its history is the `.versions/` tree.
    fn list_snapshots(&self, job_name: &str) -> Result<Vec<SnapshotInfo>>;

    /// Apply retention rules. Rustic reads the `keep_*` count fields
    /// (`keep_last`, `keep_daily`, …) over snapshots; mirror reads
    /// `keep_versions` and prunes `.versions/`.
    fn apply_retention(
        &self,
        job_name: &str,
        retention: &Retention,
    ) -> Result<RetentionOutcome>;

    /// Restore the latest state of `job_name` into `dest_dir`.
    ///
    /// For rustic: the most recent snapshot tagged
    /// `kovre-job:<job_name>`. For mirror: the current canonical
    /// state (`.versions/` is ignored).
    ///
    /// `dest_dir` is created if missing. Existing contents are left
    /// in place — restored files overwrite their counterparts, but
    /// extra files in `dest_dir` that aren't in the backup are
    /// preserved.
    ///
    /// Returns an error if the repository has no state to restore
    /// for this job (no snapshots / no mirrored files).
    fn restore_latest(&self, job_name: &str, dest_dir: &std::path::Path) -> Result<()>;
}

/// Pick the right engine for a repository, based on its `backend:`
/// declaration in `kovre.yaml`.
pub fn engine_for(repo: &RepoConfig) -> Box<dyn BackupEngine> {
    match repo.backend {
        BackendKind::Rustic => Box::new(RusticEngine::new(repo.clone())),
        BackendKind::Mirror => Box::new(MirrorEngine::new(repo.clone())),
    }
}
