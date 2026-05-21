//! Lifecycle of a `RestoreRun` triggered from the dashboard.
//!
//! Calque sur `serve::runs` : pure-policy functions on top of the
//! Lithair `DeclarativeHttpHandler<RestoreRun>`, plus an orchestrating
//! `trigger_restore` that spawns the actual filesystem work.
//!
//! Concurrency rule: only one restore can be `running` per
//! `(job_name, dest_dir)` pair at a time. Two restores into the same
//! destination would race for the same files; two into different dests
//! are independent and explicitly allowed.

use std::path::PathBuf;
use std::sync::Arc;

use kovre_core::backup::{self};
use kovre_core::config::{Config, Job, Repository};
use lithair_core::http::DeclarativeHttpHandler;
use serde::Serialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::serve::models::RestoreRun;

/// Why a `POST /api/jobs/:name/restore` could not be accepted.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum RestoreError {
    /// `job_name` does not appear in `kovre.yaml::jobs`.
    UnknownJob { job: String },
    /// `dest_dir` is empty or contains `..` (path traversal).
    InvalidDest { reason: String },
    /// Another restore for the same `(job, dest_dir)` is already
    /// running.
    AlreadyRunning { run_id: String },
    /// The Lithair handler refused the persistence write.
    Persistence { reason: String },
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownJob { job } => write!(f, "unknown job `{job}`"),
            Self::InvalidDest { reason } => write!(f, "invalid dest_dir: {reason}"),
            Self::AlreadyRunning { run_id } => {
                write!(f, "a restore is already in progress (id = {run_id})")
            }
            Self::Persistence { reason } => write!(f, "persistence error: {reason}"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// Reject empty paths and any segment equal to `..`. Returns the
/// trimmed input on success so the caller works with a canonical
/// value. We don't canonicalize for real — the destination may not
/// yet exist on disk, and `std::fs::canonicalize` fails on missing
/// paths.
pub fn validate_dest_dir(raw: &str) -> Result<String, RestoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(RestoreError::InvalidDest {
            reason: "dest_dir is empty".into(),
        });
    }
    if trimmed.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(RestoreError::InvalidDest {
            reason: "path traversal segment `..` not allowed".into(),
        });
    }
    Ok(trimmed.to_string())
}

/// Insert a fresh `running` `RestoreRun` for `(job_name, dest_dir)`.
///
/// Validates the job exists, validates `dest_dir`, refuses if another
/// restore is already running for the same `(job, dest)` pair, and
/// persists the new entry via `apply_replicated_item`.
pub async fn register_restore_run(
    handler: &DeclarativeHttpHandler<RestoreRun>,
    cfg: &Config,
    job_name: &str,
    dest_dir_raw: &str,
    trigger: &str,
) -> Result<RestoreRun, RestoreError> {
    if !cfg.jobs.contains_key(job_name) {
        return Err(RestoreError::UnknownJob {
            job: job_name.to_string(),
        });
    }
    let dest_dir = validate_dest_dir(dest_dir_raw)?;

    let job_name_owned = job_name.to_string();
    let dest_for_query = dest_dir.clone();
    let existing_running: Vec<RestoreRun> = handler
        .query(|r| {
            r.job_name == job_name_owned && r.dest_dir == dest_for_query && r.status == "running"
        })
        .await;
    if let Some(r) = existing_running.into_iter().next() {
        return Err(RestoreError::AlreadyRunning { run_id: r.id });
    }

    let run = RestoreRun {
        id: Uuid::new_v4().hyphenated().to_string(),
        job_name: job_name.to_string(),
        dest_dir,
        started_at: now_rfc3339(),
        finished_at: None,
        status: "running".into(),
        failure_reason: None,
        trigger: trigger.to_string(),
    };

    handler
        .apply_replicated_item(run.clone())
        .await
        .map_err(|reason| RestoreError::Persistence { reason })?;

    Ok(run)
}

/// Transition a `running` restore to `success`.
pub async fn mark_restore_success(
    handler: &DeclarativeHttpHandler<RestoreRun>,
    run_id: &str,
) -> Result<RestoreRun, String> {
    let mut current = handler
        .get_by_id(run_id)
        .await
        .ok_or_else(|| format!("restore run `{run_id}` not found"))?;
    current.status = "success".into();
    current.finished_at = Some(now_rfc3339());
    handler.apply_replicated_update(run_id, current.clone()).await?;
    Ok(current)
}

/// Transition a `running` restore to `failed`, recording the reason.
pub async fn mark_restore_failure(
    handler: &DeclarativeHttpHandler<RestoreRun>,
    run_id: &str,
    reason: &str,
) -> Result<RestoreRun, String> {
    let mut current = handler
        .get_by_id(run_id)
        .await
        .ok_or_else(|| format!("restore run `{run_id}` not found"))?;
    current.status = "failed".into();
    current.failure_reason = Some(reason.to_string());
    current.finished_at = Some(now_rfc3339());
    handler.apply_replicated_update(run_id, current.clone()).await?;
    Ok(current)
}

/// Full orchestration of a triggered restore.
///
/// 1. Validates + registers the restore synchronously and returns its
///    id to the caller (so the HTTP layer can answer 202 immediately).
/// 2. Spawns a Tokio task that runs the restore via `spawn_blocking`
///    (engine API is sync) and updates the run on completion.
//
// step 1 of Phase 6 only ships the module + model + helpers; the
// caller (`POST /api/jobs/:name/restore` in `serve::mod`) lands in
// step 2. The function below is otherwise unused for one PR's worth
// of time — silence the unused-fn warnings until then.
#[allow(dead_code)]
pub async fn trigger_restore(
    handler: Arc<DeclarativeHttpHandler<RestoreRun>>,
    cfg: Arc<Config>,
    job_name: String,
    dest_dir: String,
    trigger: String,
) -> Result<String, RestoreError> {
    let run = register_restore_run(&handler, &cfg, &job_name, &dest_dir, &trigger).await?;
    let run_id = run.id.clone();

    let job = cfg
        .jobs
        .get(&job_name)
        .expect("validated by register_restore_run")
        .clone();
    let repo = cfg.repositories.get(&job.repository).cloned();
    let dest_dir_owned = run.dest_dir.clone();

    let handler_for_task = Arc::clone(&handler);
    tokio::spawn(async move {
        let outcome = run_restore(&job_name, &job, repo, &dest_dir_owned).await;
        match outcome {
            Ok(()) => {
                info!(
                    run_id = %run.id,
                    dest = %dest_dir_owned,
                    "restore completed"
                );
                if let Err(persist_err) = mark_restore_success(&handler_for_task, &run.id).await {
                    warn!(run_id = %run.id, "failed to persist success: {persist_err}");
                }
            }
            Err(err) => {
                let reason = format!("{err:#}");
                error!(run_id = %run.id, "restore failed: {reason}");
                if let Err(persist_err) =
                    mark_restore_failure(&handler_for_task, &run.id, &reason).await
                {
                    warn!(run_id = %run.id, "failed to persist failure: {persist_err}");
                }
            }
        }
    });

    Ok(run_id)
}

/// Resolve the repo and call `engine.restore_latest`. Wrapped in
/// `spawn_blocking` because the rustic restore path can read tens of
/// GB and we don't want to monopolize the Tokio reactor.
#[allow(dead_code)] // wired up by `trigger_restore` (see above).
async fn run_restore(
    job_name: &str,
    job: &Job,
    repo: Option<Repository>,
    dest_dir: &str,
) -> anyhow::Result<()> {
    let repo = repo.ok_or_else(|| {
        anyhow::anyhow!(
            "job `{job_name}` references unknown repository `{}`",
            job.repository
        )
    })?;
    let job_name_owned = job_name.to_string();
    let dest_path = PathBuf::from(dest_dir);
    tokio::task::spawn_blocking(move || {
        backup::engine_for(&repo).restore_latest(&job_name_owned, &dest_path)
    })
    .await
    .map_err(|e| anyhow::anyhow!("restore task panicked: {e}"))?
}

fn now_rfc3339() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use kovre_core::config::{Agent, BackendKind, Job, Repository as RepoConfig};
    use tempfile::TempDir;

    async fn make_handler() -> (Arc<DeclarativeHttpHandler<RestoreRun>>, TempDir) {
        let tempdir = TempDir::new().unwrap();
        let handler = DeclarativeHttpHandler::<RestoreRun>::new_with_replay(
            tempdir.path().to_str().unwrap(),
        )
        .await
        .expect("handler init");
        (Arc::new(handler), tempdir)
    }

    fn cfg_with(job_names: &[&str]) -> Config {
        let mut repositories: IndexMap<String, RepoConfig> = IndexMap::new();
        repositories.insert(
            "nas".into(),
            RepoConfig {
                path: PathBuf::from(r"\\nas\backup"),
                backend: BackendKind::Mirror,
                password_file: None,
            },
        );
        let mut jobs: IndexMap<String, Job> = IndexMap::new();
        for n in job_names {
            jobs.insert(
                (*n).into(),
                Job {
                    repository: "nas".into(),
                    template: None,
                    template_options: None,
                    paths: Some(vec![PathBuf::from(r"D:\src")]),
                    excludes: None,
                    retention: None,
                },
            );
        }
        Config {
            agent: Agent {
                data_dir: PathBuf::from(r"C:\ProgramData\Kovre"),
                log_level: "info".into(),
            },
            repositories,
            jobs,
        }
    }

    #[test]
    fn validate_dest_accepts_a_plain_path() {
        let s = validate_dest_dir(r"C:\Users\me\kovre-restore\j\2026-05-20").unwrap();
        assert!(s.contains("kovre-restore"));
    }

    #[test]
    fn validate_dest_rejects_empty_input() {
        let err = validate_dest_dir("   ").unwrap_err();
        match err {
            RestoreError::InvalidDest { reason } => assert!(reason.contains("empty")),
            _ => panic!("wrong error variant: {err:?}"),
        }
    }

    #[test]
    fn validate_dest_rejects_path_traversal() {
        let err = validate_dest_dir(r"C:\Users\..\Windows").unwrap_err();
        match err {
            RestoreError::InvalidDest { reason } => assert!(reason.contains("path traversal")),
            _ => panic!("wrong error variant: {err:?}"),
        }
    }

    #[test]
    fn validate_dest_rejects_forward_slash_traversal() {
        let err = validate_dest_dir("/home/me/../etc/passwd").unwrap_err();
        assert!(matches!(err, RestoreError::InvalidDest { .. }));
    }

    #[tokio::test]
    async fn register_restore_rejects_unknown_job() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let err = register_restore_run(&h, &cfg, "ghost", r"C:\restore", "dashboard")
            .await
            .unwrap_err();
        assert!(matches!(err, RestoreError::UnknownJob { .. }));
    }

    #[tokio::test]
    async fn register_restore_rejects_invalid_dest() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let err = register_restore_run(&h, &cfg, "files", r"  ", "dashboard")
            .await
            .unwrap_err();
        assert!(matches!(err, RestoreError::InvalidDest { .. }));
    }

    #[tokio::test]
    async fn register_restore_persists_running_state() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let run = register_restore_run(&h, &cfg, "files", r"C:\restore\one", "dashboard")
            .await
            .unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.job_name, "files");
        assert_eq!(run.dest_dir, r"C:\restore\one");
        let got = h.get_by_id(&run.id).await.unwrap();
        assert_eq!(got.id, run.id);
    }

    #[tokio::test]
    async fn register_restore_refuses_second_concurrent_restore_for_same_pair() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let first = register_restore_run(&h, &cfg, "files", r"C:\dst", "dashboard")
            .await
            .unwrap();
        let err = register_restore_run(&h, &cfg, "files", r"C:\dst", "dashboard")
            .await
            .unwrap_err();
        match err {
            RestoreError::AlreadyRunning { run_id } => assert_eq!(run_id, first.id),
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn register_restore_allows_concurrent_restores_to_different_dests() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        register_restore_run(&h, &cfg, "files", r"C:\dst1", "dashboard")
            .await
            .unwrap();
        // Different dest → allowed.
        register_restore_run(&h, &cfg, "files", r"C:\dst2", "dashboard")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mark_restore_success_transitions_state() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let run = register_restore_run(&h, &cfg, "files", r"C:\dst", "dashboard")
            .await
            .unwrap();
        let after = mark_restore_success(&h, &run.id).await.unwrap();
        assert_eq!(after.status, "success");
        assert!(after.finished_at.is_some());
    }

    #[tokio::test]
    async fn mark_restore_failure_records_reason() {
        let (h, _td) = make_handler().await;
        let cfg = cfg_with(&["files"]);
        let run = register_restore_run(&h, &cfg, "files", r"C:\dst", "dashboard")
            .await
            .unwrap();
        let after = mark_restore_failure(&h, &run.id, "disk full").await.unwrap();
        assert_eq!(after.status, "failed");
        assert_eq!(after.failure_reason.as_deref(), Some("disk full"));
    }
}
