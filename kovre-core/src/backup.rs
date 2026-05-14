//! Backup engine abstraction.
//!
//! Phase 4 introduces a `BackupEngine` trait so the runtime can pick
//! between several persistence formats per repository. The previous
//! top-level free functions (`init_repo`, `backup_job`, …) are now
//! methods on a `RusticEngine` impl; a second `MirrorEngine` lands in
//! step 3 of the phase. Callers obtain an engine via the
//! [`engine_for`] factory, which inspects the repository's declared
//! backend.
//!
//! All snapshots produced by `RusticEngine` are tagged
//! `kovre-job:<job-name>` so a shared repository can host several
//! jobs without their snapshot lists overlapping.

use std::path::PathBuf;

use anyhow::{Context, Result};
use jiff::Zoned;
use rustic_backend::BackendOptions;
use rustic_core::{
    repofile::SnapshotFile, BackupOptions, ConfigOptions, Credentials, KeepOptions, KeyOptions,
    PathList, Repository, RepositoryBackends, RepositoryOptions, SnapshotOptions,
};
use tracing::{info, warn};

use crate::config::{BackendKind, Repository as RepoConfig, Retention};

/// Tag prefix kovre attaches to every rustic snapshot so we can later
/// filter by job. Mirror snapshots don't carry this tag (the engine
/// does not produce snapshots in the rustic sense).
pub const JOB_TAG_PREFIX: &str = "kovre-job:";

/// What kovre wants to back up: a list of source paths plus exclude globs.
#[derive(Debug, Clone)]
pub struct BackupSource {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

/// Domain-level summary of a snapshot — kept independent of rustic types so
/// the CLI (and the dashboard) doesn't have to import `rustic_core`.
/// Engines that don't have a notion of snapshot (the mirror backend)
/// fabricate one per backup run from the `JobRun` metadata.
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
/// Implementations are expected to be cheap to construct (they hold a
/// reference to the `RepoConfig`, no I/O at construction time) but may
/// be long-running on the actual operations.
pub trait BackupEngine: Send + Sync {
    /// Materialize the repository on disk. For rustic, runs `init`. For
    /// mirror (Phase 4 step 3), creates the destination directory.
    /// Engines that detect an already-initialized repo return an error
    /// the caller can match on (rustic surfaces "config file already
    /// exists"); the dashboard treats that as 409 `already_initialized`.
    fn init(&self) -> Result<()>;

    /// Run a backup for `job_name` against `source`. Returns a snapshot
    /// summary (synthesized for engines without a native snapshot
    /// concept).
    fn backup(&self, job_name: &str, source: BackupSource) -> Result<SnapshotInfo>;

    /// Enumerate the snapshots known for this job. Mirror returns an
    /// empty vec — its history lives in `.versions/` rather than as
    /// discrete snapshots.
    fn list_snapshots(&self, job_name: &str) -> Result<Vec<SnapshotInfo>>;

    /// Apply retention rules. Rustic interprets the `keep_*` count
    /// fields (`keep_last`, `keep_daily`, …) over snapshots; mirror
    /// reads `keep_versions` and prunes its `.versions/` tree.
    fn apply_retention(
        &self,
        job_name: &str,
        retention: &Retention,
    ) -> Result<RetentionOutcome>;
}

/// Pick the right engine for a repository, based on its `backend:`
/// declaration in `kovre.yaml`.
///
/// `BackendKind::Mirror` returns a placeholder that fails every
/// operation with a clear "not implemented yet" message until step 3
/// lands the real `MirrorEngine`. This way a config that declares
/// `backend: mirror` parses cleanly today, but a user who tries to
/// run a backup against it gets actionable feedback rather than
/// silent fall-back to rustic.
pub fn engine_for(repo: &RepoConfig) -> Box<dyn BackupEngine> {
    match repo.backend {
        BackendKind::Rustic => Box::new(RusticEngine::new(repo.clone())),
        BackendKind::Mirror => Box::new(MirrorEnginePlaceholder),
    }
}

/// Stand-in for the mirror engine while Phase 4 step 3 is in flight.
/// Every method returns the same error so callers know which step
/// to wait for.
struct MirrorEnginePlaceholder;

impl BackupEngine for MirrorEnginePlaceholder {
    fn init(&self) -> Result<()> {
        anyhow::bail!(
            "the `mirror` backend lands in Phase 4 step 3 — keep `backend: rustic` for now"
        )
    }
    fn backup(&self, _job_name: &str, _source: BackupSource) -> Result<SnapshotInfo> {
        anyhow::bail!("mirror backend not implemented yet (Phase 4 step 3)")
    }
    fn list_snapshots(&self, _job_name: &str) -> Result<Vec<SnapshotInfo>> {
        // Return empty — the dashboard's snapshot sync should never
        // abort just because a mirror repo is configured.
        Ok(Vec::new())
    }
    fn apply_retention(
        &self,
        _job_name: &str,
        _retention: &Retention,
    ) -> Result<RetentionOutcome> {
        Ok(RetentionOutcome::default())
    }
}

// ---------------------------------------------------------------------
// RusticEngine
// ---------------------------------------------------------------------

/// Backup engine backed by `rustic_core`. Stores deduplicated,
/// encrypted, restic-compatible snapshots at the configured path.
pub struct RusticEngine {
    repo: RepoConfig,
}

impl RusticEngine {
    pub fn new(repo: RepoConfig) -> Self {
        Self { repo }
    }

    fn read_password(&self) -> Result<String> {
        let password_file = self
            .repo
            .password_file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(
                "rustic backend requires `password_file:` on the repository — set it in kovre.yaml \
                 (or pick `backend: mirror` if you don't want a passphrase)"
            ))?;
        let raw = std::fs::read_to_string(password_file)
            .with_context(|| format!("reading password file `{}`", password_file.display()))?;
        let pass = raw.lines().next().unwrap_or("").trim_end().to_string();
        if pass.is_empty() {
            anyhow::bail!("password file `{}` is empty", password_file.display());
        }
        Ok(pass)
    }

    fn make_backends(&self) -> Result<RepositoryBackends> {
        let path = self.repo.path.to_string_lossy().to_string();
        BackendOptions::default()
            .repository(path.clone())
            .to_backends()
            .with_context(|| format!("opening backend at `{path}`"))
    }

    fn credentials(&self) -> Result<Credentials> {
        Ok(Credentials::Password(self.read_password()?))
    }
}

impl BackupEngine for RusticEngine {
    fn init(&self) -> Result<()> {
        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .init(&creds, &KeyOptions::default(), &ConfigOptions::default())
            .context("initializing repository on backend")?;

        info!(repository = %self.repo.path.display(), "repository initialized");
        Ok(())
    }

    fn backup(&self, job_name: &str, source: BackupSource) -> Result<SnapshotInfo> {
        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        let repository = Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .open(&creds)
            .context("opening repository (wrong password?)")?
            .to_indexed_ids()
            .context("loading repository index")?;

        let mut backup_opts = BackupOptions::default();
        // rustic's `Excludes::globs` are passed to `ignore::OverrideBuilder`,
        // which uses *whitelist* semantics — bare patterns mean "include
        // only matches, exclude everything else". We expose the more
        // intuitive "exclude these" semantics (matching the YAML field
        // name `excludes:` and restic conventions), so we negate every
        // pattern.
        backup_opts.excludes.globs = source
            .excludes
            .into_iter()
            .map(|p| if p.starts_with('!') { p } else { format!("!{p}") })
            .collect();

        // Drop paths that don't exist — common in template-resolved
        // job sources (Steam saves for games never launched, Pictures
        // on a fresh Windows install). `PathList::sanitize` calls
        // `canonicalize` which is fail-all-or-nothing.
        let existing: Vec<PathBuf> = source
            .paths
            .into_iter()
            .filter(|p| {
                let ok = p.exists();
                if !ok {
                    warn!(path = %p.display(), "source path does not exist, skipping");
                }
                ok
            })
            .collect();
        if existing.is_empty() {
            anyhow::bail!("no source paths exist on this system — nothing to back up");
        }

        let pathlist: PathList = existing.iter().cloned().collect();
        let pathlist = pathlist.sanitize().context("sanitizing source paths")?;

        let snap = SnapshotOptions::default()
            .add_tags(&format!("{JOB_TAG_PREFIX}{job_name}"))
            .context("setting snapshot tag")?
            .to_snapshot()
            .context("building snapshot record")?;

        info!(job = job_name, paths = ?existing, "starting backup");
        let snap = repository
            .backup(&backup_opts, &pathlist, snap)
            .context("running backup")?;
        info!(
            job = job_name,
            snapshot = %snap.id,
            bytes = snap.summary.as_ref().map(|s| s.total_bytes_processed),
            "backup complete"
        );

        Ok(snap_to_info(snap))
    }

    fn list_snapshots(&self, job_name: &str) -> Result<Vec<SnapshotInfo>> {
        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        let repository = Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .open(&creds)
            .context("opening repository (wrong password?)")?;

        let target_tag = format!("{JOB_TAG_PREFIX}{job_name}");
        let mut snaps: Vec<SnapshotInfo> = repository
            .get_all_snapshots()
            .context("listing snapshots")?
            .into_iter()
            .filter(|s| s.tags.contains(&target_tag))
            .map(snap_to_info)
            .collect();
        // Newest first — `SnapshotFile::time` formats RFC3339, which is
        // lexically chronological.
        snaps.sort_by(|a, b| b.time.cmp(&a.time));
        Ok(snaps)
    }

    fn apply_retention(
        &self,
        job_name: &str,
        retention: &Retention,
    ) -> Result<RetentionOutcome> {
        let keep = build_keep_options(retention);
        if !any_rule_set(retention) {
            return Ok(RetentionOutcome::default());
        }

        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        let repository = Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .open(&creds)
            .context("opening repository (wrong password?)")?;

        let target_tag = format!("{JOB_TAG_PREFIX}{job_name}");
        let snapshots: Vec<SnapshotFile> = repository
            .get_all_snapshots()
            .context("listing snapshots for retention")?
            .into_iter()
            .filter(|s| s.tags.contains(&target_tag))
            .collect();

        if snapshots.is_empty() {
            return Ok(RetentionOutcome::default());
        }

        let now = Zoned::now();
        let evaluated = keep
            .apply(snapshots, &now)
            .context("evaluating retention rules")?;

        let mut to_forget = Vec::new();
        let mut kept = 0usize;
        for s in &evaluated {
            if s.keep {
                kept += 1;
            } else {
                to_forget.push(s.snapshot.id);
            }
        }

        if !to_forget.is_empty() {
            repository
                .delete_snapshots(&to_forget)
                .context("deleting forgotten snapshots")?;
        }

        Ok(RetentionOutcome {
            kept,
            forgotten: to_forget.len(),
        })
    }
}

// ---------------------------------------------------------------------
// rustic helpers (private)
// ---------------------------------------------------------------------

fn build_keep_options(r: &Retention) -> KeepOptions {
    KeepOptions::default()
        .keep_last(r.keep_last.map(|v| v as i32))
        .keep_hourly(r.keep_hourly.map(|v| v as i32))
        .keep_daily(r.keep_daily.map(|v| v as i32))
        .keep_weekly(r.keep_weekly.map(|v| v as i32))
        .keep_monthly(r.keep_monthly.map(|v| v as i32))
        .keep_yearly(r.keep_yearly.map(|v| v as i32))
}

fn any_rule_set(r: &Retention) -> bool {
    r.keep_last.is_some()
        || r.keep_hourly.is_some()
        || r.keep_daily.is_some()
        || r.keep_weekly.is_some()
        || r.keep_monthly.is_some()
        || r.keep_yearly.is_some()
}

fn snap_to_info(s: SnapshotFile) -> SnapshotInfo {
    SnapshotInfo {
        id: s.id.to_string(),
        time: s.time.to_string(),
        paths: s.paths.iter().cloned().collect(),
        hostname: s.hostname,
        tags: s.tags.iter().cloned().collect(),
        total_bytes_processed: s.summary.as_ref().map(|sum| sum.total_bytes_processed),
        data_added: s.summary.as_ref().map(|sum| sum.data_added),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_keep_options_maps_all_fields() {
        let r = Retention {
            keep_last: Some(7),
            keep_hourly: Some(24),
            keep_daily: Some(30),
            keep_weekly: Some(8),
            keep_monthly: Some(12),
            keep_yearly: Some(5),
            keep_versions: None,
        };
        let k = build_keep_options(&r);
        assert_eq!(k.keep_last, Some(7));
        assert_eq!(k.keep_hourly, Some(24));
        assert_eq!(k.keep_daily, Some(30));
        assert_eq!(k.keep_weekly, Some(8));
        assert_eq!(k.keep_monthly, Some(12));
        assert_eq!(k.keep_yearly, Some(5));
    }

    #[test]
    fn build_keep_options_leaves_unset_fields_none() {
        let r = Retention {
            keep_last: Some(3),
            ..Default::default()
        };
        let k = build_keep_options(&r);
        assert_eq!(k.keep_last, Some(3));
        assert_eq!(k.keep_daily, None);
        assert_eq!(k.keep_weekly, None);
    }

    #[test]
    fn any_rule_set_detects_at_least_one_field() {
        assert!(!any_rule_set(&Retention::default()));
        assert!(any_rule_set(&Retention {
            keep_last: Some(1),
            ..Default::default()
        }));
        assert!(any_rule_set(&Retention {
            keep_yearly: Some(1),
            ..Default::default()
        }));
    }

    #[test]
    fn engine_for_returns_rustic_engine_by_default() {
        use std::path::PathBuf;
        let repo = RepoConfig {
            path: PathBuf::from(r"C:\nope"),
            backend: BackendKind::Rustic,
            password_file: Some(PathBuf::from(r"C:\nope.key")),
        };
        let _engine = engine_for(&repo); // boxed trait object; just check it constructs
    }

    #[test]
    fn engine_for_returns_placeholder_for_mirror() {
        use std::path::PathBuf;
        let repo = RepoConfig {
            path: PathBuf::from(r"C:\nope"),
            backend: BackendKind::Mirror,
            password_file: None,
        };
        let engine = engine_for(&repo);
        // The placeholder errors on init/backup but not on the read
        // methods — those return empty so the dashboard's snapshot
        // sync doesn't blow up over a mirror repo before step 3.
        assert!(engine.init().is_err());
        assert!(engine.backup("any", BackupSource { paths: vec![], excludes: vec![] }).is_err());
        assert!(engine.list_snapshots("any").unwrap().is_empty());
        assert_eq!(
            engine.apply_retention("any", &Retention::default()).unwrap().forgotten,
            0
        );
    }

    #[test]
    fn rustic_engine_reports_missing_password_file() {
        use std::path::PathBuf;
        let repo = RepoConfig {
            path: PathBuf::from(r"C:\nope"),
            backend: BackendKind::Rustic,
            password_file: None,
        };
        let engine = RusticEngine::new(repo);
        let err = engine.init().unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("password_file"),
            "expected hint about password_file in error message, got: {msg}"
        );
    }
}
