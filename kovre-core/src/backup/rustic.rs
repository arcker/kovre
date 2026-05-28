//! `BackupEngine` impl backed by `rustic_core` — deduplicated,
//! encrypted, restic-compatible storage.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Zoned;
use rustic_backend::BackendOptions;
use rustic_core::{
    repofile::SnapshotFile, BackupOptions, CheckOptions, ConfigOptions, Credentials, KeepOptions,
    KeyOptions, LocalDestination, LsOptions, PathList, Repository, RepositoryBackends,
    RepositoryOptions, RestoreOptions, SnapshotOptions,
};
use tracing::{info, warn};

use crate::backup::{
    BackupEngine, BackupSource, BrowseEntry, RetentionOutcome, SnapshotInfo, VerifyOutcome,
    VersionInfo, JOB_TAG_PREFIX,
};
use crate::config::{Repository as RepoConfig, Retention};

/// Backup engine backed by `rustic_core`.
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
        // rustic's `Excludes::globs` uses whitelist semantics — bare
        // patterns mean "include only matches". We expose the more
        // intuitive "exclude these" semantics (matching restic), so we
        // prefix every pattern with `!`.
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

    fn restore_latest(&self, job_name: &str, dest_dir: &Path) -> Result<()> {
        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        let repository = Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .open(&creds)
            .context("opening repository (wrong password?)")?
            .to_indexed()
            .context("loading repository index")?;

        let target_tag = format!("{JOB_TAG_PREFIX}{job_name}");
        let snapshot = repository
            .get_all_snapshots()
            .context("listing snapshots")?
            .into_iter()
            .filter(|s| s.tags.contains(&target_tag))
            // Pick the newest by snapshot time. `SnapshotFile::time`
            // is a chronological string but the field is jiff-typed
            // and `Ord`-able, so we can compare directly.
            .max_by(|a, b| a.time.cmp(&b.time))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no snapshots found for job `{job_name}` (tag `{target_tag}`)"
                )
            })?;

        std::fs::create_dir_all(dest_dir).with_context(|| {
            format!("creating restore destination `{}`", dest_dir.display())
        })?;
        let dest_str = dest_dir.to_string_lossy().to_string();
        let dest = LocalDestination::new(&dest_str, true, false)
            .context("opening restore destination")?;

        let root_node = repository
            .node_from_snapshot_and_path(&snapshot, "")
            .context("locating snapshot root node")?;
        let ls_opts = LsOptions::default();
        let streamer = repository
            .ls(&root_node, &ls_opts)
            .context("streaming snapshot tree")?;

        let restore_opts = RestoreOptions::default();
        let plan = repository
            .prepare_restore(&restore_opts, streamer.clone(), &dest, false)
            .context("preparing restore plan")?;
        repository
            .restore(plan, &restore_opts, streamer, &dest)
            .context("executing restore")?;

        info!(
            job = job_name,
            snapshot = %snapshot.id,
            dest = %dest_dir.display(),
            "rustic restore complete"
        );
        Ok(())
    }

    fn verify(&self) -> Result<VerifyOutcome> {
        let backends = self.make_backends()?;
        let creds = self.credentials()?;
        let opts = RepositoryOptions::default();

        let repository = Repository::new(&opts, &backends)
            .context("creating repository handle")?
            .open(&creds)
            .context("opening repository (wrong password?)")?;

        // Defaults: metadata + index walk; no pack re-read. That's
        // the right tradeoff for a UI "Verify" button — fast enough
        // to run on demand without a progress UI, but still catches
        // corruption in index, pack references, snapshot trees.
        let results = repository
            .check(CheckOptions::default())
            .context("running rustic check")?;

        let messages: Vec<String> = results
            .0
            .iter()
            .map(|(level, err)| format!("[{level:?}] {err}"))
            .collect();
        let ok = results.is_ok().is_ok();

        info!(
            repository = %self.repo.path.display(),
            ok,
            findings = messages.len(),
            "rustic verify complete"
        );

        Ok(VerifyOutcome { ok, messages })
    }

    fn browse(&self, _job_name: &str, _subpath: &str) -> Result<Vec<BrowseEntry>> {
        anyhow::bail!(
            "browse is not supported for the rustic backend — content is encrypted \
             and deduplicated. Use `rustic ls <snapshot>` CLI to inspect snapshot contents."
        )
    }

    fn list_versions(&self, _job_name: &str, _rel_path: &str) -> Result<Vec<VersionInfo>> {
        anyhow::bail!(
            "version history is not available for the rustic backend — \
             rustic uses immutable snapshots, not per-file versioning."
        )
    }

    fn restore_selective(
        &self,
        job_name: &str,
        dest_dir: &Path,
        _subpath: Option<&str>,
    ) -> Result<()> {
        // Rustic doesn't support partial restore via rustic_core easily;
        // fall back to full restore.
        self.restore_latest(job_name, dest_dir)
    }
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
    use crate::config::BackendKind;

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
    fn rustic_engine_reports_missing_password_file() {
        use std::path::PathBuf;
        let repo = RepoConfig {
            path: PathBuf::from(r"C:\nope"),
            backend: BackendKind::Rustic,
            password_file: None,
        smb_user: None,
        smb_password_file: None,
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
