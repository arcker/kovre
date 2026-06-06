//! Pull data from the authoritative source (rustic repos) into the
//! Lithair-backed dashboard projection.
//!
//! Currently only refreshes the `Snapshot` model. The `Repository` and
//! `Job` definitions stay outside Lithair (they live in `kovre.yaml`).

use kovre_core::backup::{self, SnapshotInfo};
use kovre_core::config::Config;
use lithair_core::http::DeclarativeHttpHandler;
use tracing::{debug, info, warn};

use crate::serve::models::Snapshot;

/// Walk every job declared in `cfg`, ask rustic for its snapshots, and
/// upsert each into the `Snapshot` projection.
///
/// Idempotent: relies on `apply_replicated_update`'s create-or-update
/// semantics so a re-run after restart (the event log already replayed
/// past inserts) does not produce duplicate-id errors.
///
/// A repository that fails to open (offline NAS, wrong password file,
/// uninitialized repo) is logged and skipped; other jobs continue.
/// Returns the total number of snapshots successfully synced.
pub async fn sync_snapshots(
    handler: &DeclarativeHttpHandler<Snapshot>,
    cfg: &Config,
) -> usize {
    let mut total_synced = 0usize;

    for (job_name, job) in &cfg.jobs {
        let Some(repo) = cfg.repositories.get(&job.repository) else {
            warn!(
                job = job_name,
                repository = %job.repository,
                "snapshot sync: skipping (repository not declared in kovre.yaml)"
            );
            continue;
        };

        // `BackupEngine::list_snapshots` is sync (rustic_core's API
        // underneath); push it off the reactor.
        let job_name_owned = job_name.clone();
        let repo_owned = repo.clone();
        let result = tokio::task::spawn_blocking(move || {
            backup::engine_for(&repo_owned).list_snapshots(&job_name_owned)
        })
        .await;

        let snapshots = match result {
            Ok(Ok(snaps)) => snaps,
            Ok(Err(err)) => {
                warn!(
                    job = job_name,
                    "snapshot sync: rustic refused to list snapshots: {err:#}"
                );
                continue;
            }
            Err(join_err) => {
                warn!(job = job_name, "snapshot sync: blocking task panicked: {join_err}");
                continue;
            }
        };

        let count_before = total_synced;
        for snap in snapshots {
            let model = snapshot_from_info(job_name, snap);
            match handler.apply_replicated_update(&model.id.clone(), model).await {
                Ok(()) => total_synced += 1,
                Err(persist_err) => warn!(
                    job = job_name,
                    "snapshot sync: failed to persist snapshot: {persist_err}"
                ),
            }
        }

        debug!(
            job = job_name,
            synced = total_synced - count_before,
            "snapshot sync: done"
        );
    }

    info!(snapshots = total_synced, "snapshot sync: complete");
    total_synced
}

/// Project a rustic-level `SnapshotInfo` into the Lithair-level model.
fn snapshot_from_info(job_name: &str, info: SnapshotInfo) -> Snapshot {
    Snapshot {
        id: info.id,
        job_name: job_name.to_string(),
        time: info.time,
        paths: info.paths,
        hostname: info.hostname,
        bytes_total: info.total_bytes_processed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use kovre_core::backup::{self as kbackup, BackupSource};
    use kovre_core::config::{Agent, Job, Repository};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Materialize a real rustic repository with one snapshot tagged
    /// `kovre-job:<job_name>` so `sync_snapshots` has something to find.
    /// Returns the workspace TempDir (kept alive by the caller) plus
    /// the Config that points at it.
    fn build_real_repo_with_snapshot(job_name: &str) -> (TempDir, Config) {
        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("hello.txt"), b"hello\n").unwrap();

        let repo_path = root.join("repo");
        std::fs::create_dir_all(&repo_path).unwrap();
        let password_file = root.join("repo.key");
        std::fs::write(&password_file, "test-pass\n").unwrap();

        let repo_cfg = Repository {
            path: repo_path.clone(),
            backend: kovre_core::config::BackendKind::Rustic,
            password_file: Some(password_file.clone()),
        smb_user: None,
        smb_password_file: None,
        };

        kbackup::engine_for(&repo_cfg).init().unwrap();
        kbackup::engine_for(&repo_cfg)
            .backup(
                job_name,
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                }, None,
            )
            .unwrap();

        let mut repositories = IndexMap::new();
        repositories.insert("test".into(), repo_cfg);
        let mut jobs = IndexMap::new();
        jobs.insert(
            job_name.into(),
            Job {
                repository: "test".into(),
                template: None,
                template_options: None,
                paths: Some(vec![source]),
                excludes: None,
                retention: None,
            },
        );
        let cfg = Config {
            agent: Agent {
                data_dir: PathBuf::from(root),
                log_level: "info".into(),
            },
            repositories,
            jobs,
        };
        (workspace, cfg)
    }

    async fn make_handler() -> (Arc<DeclarativeHttpHandler<Snapshot>>, TempDir) {
        let tempdir = TempDir::new().unwrap();
        let handler =
            DeclarativeHttpHandler::<Snapshot>::new_with_replay(tempdir.path().to_str().unwrap())
                .await
                .unwrap();
        (Arc::new(handler), tempdir)
    }

    #[tokio::test]
    async fn sync_inserts_snapshots_for_known_jobs() {
        let (_workspace, cfg) = build_real_repo_with_snapshot("documents");
        let (handler, _store) = make_handler().await;

        let synced = sync_snapshots(&handler, &cfg).await;
        assert_eq!(synced, 1, "expected exactly one snapshot synced");

        let stored = handler.get_all_items().await;
        assert_eq!(stored.len(), 1);
        let s = &stored[0];
        assert_eq!(s.job_name, "documents");
        assert!(!s.id.is_empty());
        assert!(!s.paths.is_empty());
    }

    #[tokio::test]
    async fn re_sync_is_idempotent() {
        let (_workspace, cfg) = build_real_repo_with_snapshot("documents");
        let (handler, _store) = make_handler().await;

        let first = sync_snapshots(&handler, &cfg).await;
        let second = sync_snapshots(&handler, &cfg).await;

        assert_eq!(first, 1);
        assert_eq!(second, 1, "re-syncing should not error and should still return 1");

        let stored = handler.get_all_items().await;
        assert_eq!(
            stored.len(),
            1,
            "snapshot count should remain 1 after re-sync (no duplicates)"
        );
    }

    #[tokio::test]
    async fn sync_skips_jobs_with_unknown_repository() {
        let (_workspace, mut cfg) = build_real_repo_with_snapshot("documents");

        // Add a second job that points at a repository that does not exist.
        cfg.jobs.insert(
            "orphan".into(),
            Job {
                repository: "nope".into(),
                template: None,
                template_options: None,
                paths: Some(vec![PathBuf::from(r"C:\nope")]),
                excludes: None,
                retention: None,
            },
        );

        let (handler, _store) = make_handler().await;
        let synced = sync_snapshots(&handler, &cfg).await;
        // Only `documents` syncs; `orphan` is skipped silently.
        assert_eq!(synced, 1);
    }
}
