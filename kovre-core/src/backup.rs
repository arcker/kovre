//! Wrapper around `rustic_core` — the only module in kovre that knows about rustic types.
//!
//! All snapshots produced by kovre are tagged `kovre-job:<job-name>` so that
//! `list-snapshots <job>` can filter them out of a shared repository.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rustic_backend::BackendOptions;
use jiff::Zoned;
use rustic_core::{
    BackupOptions, ConfigOptions, Credentials, KeepOptions, KeyOptions, PathList, Repository,
    RepositoryBackends, RepositoryOptions, SnapshotOptions,
    repofile::SnapshotFile,
};
use tracing::{info, warn};

use crate::config::{Repository as RepoConfig, Retention};

/// Tag prefix kovre attaches to every snapshot it creates so we can later filter by job.
pub const JOB_TAG_PREFIX: &str = "kovre-job:";

/// What kovre wants to back up: a list of source paths plus exclude globs.
#[derive(Debug, Clone)]
pub struct BackupSource {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

/// Domain-level summary of a snapshot — kept independent of rustic types so the CLI
/// (and any future UI) doesn't have to import rustic_core.
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

fn read_password(repo: &RepoConfig) -> Result<String> {
    let raw = std::fs::read_to_string(&repo.password_file).with_context(|| {
        format!("reading password file `{}`", repo.password_file.display())
    })?;
    let pass = raw.lines().next().unwrap_or("").trim_end().to_string();
    if pass.is_empty() {
        anyhow::bail!(
            "password file `{}` is empty",
            repo.password_file.display()
        );
    }
    Ok(pass)
}

fn make_backends(repo: &RepoConfig) -> Result<RepositoryBackends> {
    let path = repo.path.to_string_lossy().to_string();
    BackendOptions::default()
        .repository(path.clone())
        .to_backends()
        .with_context(|| format!("opening backend at `{path}`"))
}

fn credentials(repo: &RepoConfig) -> Result<Credentials> {
    Ok(Credentials::Password(read_password(repo)?))
}

/// Initialise a brand-new repository on the configured backend.
///
/// Fails if a repository already exists at that location.
pub fn init_repo(repo: &RepoConfig) -> Result<()> {
    let backends = make_backends(repo)?;
    let creds = credentials(repo)?;
    let opts = RepositoryOptions::default();

    Repository::new(&opts, &backends)
        .context("creating repository handle")?
        .init(&creds, &KeyOptions::default(), &ConfigOptions::default())
        .context("initializing repository on backend")?;

    info!(repository = %repo.path.display(), "repository initialized");
    Ok(())
}

/// Create a snapshot of `source` in the given repository, tagged for `job_name`.
pub fn backup_job(
    repo: &RepoConfig,
    job_name: &str,
    source: BackupSource,
) -> Result<SnapshotInfo> {
    let backends = make_backends(repo)?;
    let creds = credentials(repo)?;
    let opts = RepositoryOptions::default();

    let repository = Repository::new(&opts, &backends)
        .context("creating repository handle")?
        .open(&creds)
        .context("opening repository (wrong password?)")?
        .to_indexed_ids()
        .context("loading repository index")?;

    let mut backup_opts = BackupOptions::default();
    // rustic's `Excludes::globs` are passed to `ignore::OverrideBuilder`, which uses
    // *whitelist* semantics — bare patterns mean "include only matches, exclude
    // everything else". We expose the more intuitive "exclude these" semantics to
    // users (matching the YAML field name `excludes:` and restic conventions),
    // so we negate every pattern here.
    backup_opts.excludes.globs = source
        .excludes
        .into_iter()
        .map(|p| if p.starts_with('!') { p } else { format!("!{p}") })
        .collect();

    // Drop paths that don't exist — common in template-resolved job sources
    // (e.g. Steam saves for games that have never been launched, or a Pictures
    // folder on a fresh Windows install). `PathList::sanitize` calls
    // `canonicalize` which is fail-all-or-nothing, so a single missing path
    // would otherwise abort the whole job.
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

/// List snapshots in the repository whose tag matches the given job name.
pub fn list_snapshots_for_job(repo: &RepoConfig, job_name: &str) -> Result<Vec<SnapshotInfo>> {
    let backends = make_backends(repo)?;
    let creds = credentials(repo)?;
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
    // Newest first — SnapshotFile::time is a Zoned, so we sort by the formatted RFC3339 string,
    // which is lexically chronological.
    snaps.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(snaps)
}

/// Outcome of applying retention to a single job's snapshots.
#[derive(Debug, Clone, Default)]
pub struct RetentionOutcome {
    pub kept: usize,
    pub forgotten: usize,
}

/// Apply retention rules to all snapshots tagged for `job_name`. Snapshots that
/// the rules don't keep are deleted (forgotten). Note: this does not run a
/// repository-wide prune — dead pack data remains until the user runs
/// `rustic prune` separately.
pub fn apply_retention(
    repo: &RepoConfig,
    job_name: &str,
    retention: &Retention,
) -> Result<RetentionOutcome> {
    let keep = build_keep_options(retention);
    if !any_rule_set(retention) {
        return Ok(RetentionOutcome::default());
    }

    let backends = make_backends(repo)?;
    let creds = credentials(repo)?;
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
