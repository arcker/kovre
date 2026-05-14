//! Lifecycle of a `JobRun` triggered from the dashboard.
//!
//! Splits the run flow into pure-policy functions that can be unit-tested
//! against an in-memory `DeclarativeHttpHandler<JobRun>`, and an
//! orchestrating `trigger_job_run` that spawns the actual backup work.

use std::sync::Arc;

use kovre_core::backup::{self, BackupSource, SnapshotInfo};
use kovre_core::config::{Config, Job, Repository};
use kovre_core::templates;
use lithair_core::http::DeclarativeHttpHandler;
use serde::Serialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::serve::models::JobRun;

/// Why a `POST /api/jobs/:name/run` could not be accepted.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum TriggerError {
    /// `job_name` does not appear in `kovre.yaml::jobs`.
    UnknownJob { job: String },
    /// Another run for the same job is already in `running` state.
    AlreadyRunning { run_id: String },
    /// The Lithair handler refused the persistence write.
    Persistence { reason: String },
}

impl std::fmt::Display for TriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownJob { job } => write!(f, "unknown job `{job}`"),
            Self::AlreadyRunning { run_id } => {
                write!(f, "a run is already in progress (id = {run_id})")
            }
            Self::Persistence { reason } => write!(f, "persistence error: {reason}"),
        }
    }
}

impl std::error::Error for TriggerError {}

/// Insert a fresh `running` `JobRun` for `job_name`.
///
/// Validates the job exists in `cfg`, refuses if another run is already
/// in `running` state for the same job, and persists the new entry via
/// `apply_replicated_item` (which goes through the Lithair event store).
pub async fn register_run(
    handler: &DeclarativeHttpHandler<JobRun>,
    cfg: &Config,
    job_name: &str,
    trigger: &str,
) -> Result<JobRun, TriggerError> {
    if !cfg.jobs.contains_key(job_name) {
        return Err(TriggerError::UnknownJob {
            job: job_name.to_string(),
        });
    }

    let job_name_owned = job_name.to_string();
    let existing_running: Vec<JobRun> = handler
        .query(|r| r.job_name == job_name_owned && r.status == "running")
        .await;
    if let Some(r) = existing_running.into_iter().next() {
        return Err(TriggerError::AlreadyRunning { run_id: r.id });
    }

    let run = JobRun {
        id: Uuid::new_v4().hyphenated().to_string(),
        job_name: job_name.to_string(),
        started_at: now_rfc3339(),
        finished_at: None,
        status: "running".into(),
        failure_reason: None,
        snapshot_id: None,
        bytes_processed: None,
        bytes_added: None,
        trigger: trigger.to_string(),
    };

    handler
        .apply_replicated_item(run.clone())
        .await
        .map_err(|reason| TriggerError::Persistence { reason })?;

    Ok(run)
}

/// Transition a `running` run to `success`, recording the snapshot id and
/// summary stats. Returns the updated run.
pub async fn mark_success(
    handler: &DeclarativeHttpHandler<JobRun>,
    run_id: &str,
    snapshot: &SnapshotInfo,
) -> Result<JobRun, String> {
    let mut current = handler
        .get_by_id(run_id)
        .await
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    current.status = "success".into();
    current.snapshot_id = Some(snapshot.id.clone());
    current.bytes_processed = snapshot.total_bytes_processed;
    current.bytes_added = snapshot.data_added;
    current.finished_at = Some(now_rfc3339());
    handler.apply_replicated_update(run_id, current.clone()).await?;
    Ok(current)
}

/// Transition a `running` run to `failed`, recording the reason.
pub async fn mark_failure(
    handler: &DeclarativeHttpHandler<JobRun>,
    run_id: &str,
    reason: &str,
) -> Result<JobRun, String> {
    let mut current = handler
        .get_by_id(run_id)
        .await
        .ok_or_else(|| format!("run `{run_id}` not found"))?;
    current.status = "failed".into();
    current.failure_reason = Some(reason.to_string());
    current.finished_at = Some(now_rfc3339());
    handler.apply_replicated_update(run_id, current.clone()).await?;
    Ok(current)
}

/// Full orchestration of a triggered run.
///
/// 1. Validates + registers the run synchronously and returns its id to
///    the caller (so the HTTP layer can answer 202 immediately).
/// 2. Spawns a Tokio task that runs the backup via `spawn_blocking`
///    (rustic's API is sync) and updates the run on completion.
pub async fn trigger_job_run(
    handler: Arc<DeclarativeHttpHandler<JobRun>>,
    cfg: Arc<Config>,
    job_name: String,
    trigger: String,
) -> Result<String, TriggerError> {
    let run = register_run(&handler, &cfg, &job_name, &trigger).await?;
    let run_id = run.id.clone();

    // register_run already validated `job_name` is a known job; clone the
    // job + repo definitions for the spawned task.
    let job = cfg
        .jobs
        .get(&job_name)
        .expect("validated by register_run")
        .clone();
    let repo = cfg.repositories.get(&job.repository).cloned();

    let handler_for_task = Arc::clone(&handler);
    tokio::spawn(async move {
        let outcome = run_backup(&job_name, &job, repo).await;
        match outcome {
            Ok(snapshot) => {
                info!(
                    run_id = %run.id,
                    snapshot = %snapshot.id,
                    "backup completed"
                );
                if let Err(persist_err) =
                    mark_success(&handler_for_task, &run.id, &snapshot).await
                {
                    warn!(run_id = %run.id, "failed to persist success: {persist_err}");
                }
            }
            Err(err) => {
                let reason = format!("{err:#}");
                error!(run_id = %run.id, "backup failed: {reason}");
                if let Err(persist_err) = mark_failure(&handler_for_task, &run.id, &reason).await {
                    warn!(run_id = %run.id, "failed to persist failure: {persist_err}");
                }
            }
        }
    });

    Ok(run_id)
}

/// Resolve template + run rustic backup, returning the snapshot info.
/// Wraps the synchronous `kovre_core` API in `spawn_blocking` to avoid
/// blocking the Tokio reactor.
async fn run_backup(
    job_name: &str,
    job: &Job,
    repo: Option<Repository>,
) -> anyhow::Result<SnapshotInfo> {
    let repo = repo.ok_or_else(|| {
        anyhow::anyhow!(
            "job `{job_name}` references unknown repository `{}`",
            job.repository
        )
    })?;
    let resolved = templates::resolve_job(job)?;
    if resolved.paths.is_empty() {
        anyhow::bail!("job `{job_name}` has no paths to back up");
    }
    let source = BackupSource {
        paths: resolved.paths,
        excludes: resolved.excludes,
    };
    let job_name_owned = job_name.to_string();
    tokio::task::spawn_blocking(move || {
        backup::engine_for(&repo).backup(&job_name_owned, source)
    })
    .await
    .map_err(|e| anyhow::anyhow!("backup task panicked: {e}"))?
}

/// Current time as a clean RFC 3339 string in UTC (no RFC 9557 brackets).
fn now_rfc3339() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use kovre_core::config::{Agent, Job, Repository};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fake_cfg_with_one_job() -> Config {
        use kovre_core::config::BackendKind;
        let mut repositories = IndexMap::new();
        repositories.insert(
            "test".into(),
            Repository {
                path: PathBuf::from(r"C:\nope"),
                backend: BackendKind::Rustic,
                password_file: Some(PathBuf::from(r"C:\nope.key")),
            },
        );
        let mut jobs = IndexMap::new();
        jobs.insert(
            "documents".into(),
            Job {
                repository: "test".into(),
                template: None,
                template_options: None,
                paths: Some(vec![PathBuf::from(r"C:\nope")]),
                excludes: None,
                retention: None,
            },
        );
        Config {
            agent: Agent {
                data_dir: PathBuf::from(r"C:\nope"),
                log_level: "info".into(),
            },
            repositories,
            jobs,
        }
    }

    async fn make_handler() -> (Arc<DeclarativeHttpHandler<JobRun>>, TempDir) {
        let tempdir = TempDir::new().unwrap();
        let handler = DeclarativeHttpHandler::<JobRun>::new_with_replay(
            tempdir.path().to_str().unwrap(),
        )
        .await
        .expect("handler init");
        (Arc::new(handler), tempdir)
    }

    #[tokio::test]
    async fn register_run_rejects_unknown_job() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();
        let err = register_run(&handler, &cfg, "ghost", "dashboard")
            .await
            .expect_err("expected UnknownJob");
        assert!(matches!(err, TriggerError::UnknownJob { ref job } if job == "ghost"));
    }

    #[tokio::test]
    async fn register_run_inserts_a_running_run() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();

        let run = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .expect("register_run");

        assert_eq!(run.job_name, "documents");
        assert_eq!(run.status, "running");
        assert_eq!(run.trigger, "dashboard");
        assert!(run.finished_at.is_none());
        assert!(run.snapshot_id.is_none());

        // Round-trip via the handler: the run is now persisted.
        let stored = handler.get_by_id(&run.id).await.expect("not found");
        assert_eq!(stored.id, run.id);
    }

    #[tokio::test]
    async fn register_run_rejects_concurrent_run_for_same_job() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();

        let first = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .unwrap();

        let err = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .expect_err("expected AlreadyRunning");
        match err {
            TriggerError::AlreadyRunning { run_id } => assert_eq!(run_id, first.id),
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn register_run_allows_new_run_after_previous_completed() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();

        let first = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .unwrap();
        mark_failure(&handler, &first.id, "test reason")
            .await
            .unwrap();

        // First is now status=failed; a new run should be allowed.
        let second = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .expect("second run should be accepted");
        assert_ne!(second.id, first.id);
        assert_eq!(second.status, "running");
    }

    #[tokio::test]
    async fn mark_success_records_snapshot_metadata() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();
        let run = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .unwrap();

        let snap = SnapshotInfo {
            id: "abcdef1234".into(),
            time: "2026-05-05T18:00:00Z".into(),
            paths: vec![r"C:\src".into()],
            hostname: "test-host".into(),
            tags: vec!["kovre-job:documents".into()],
            total_bytes_processed: Some(123_456),
            data_added: Some(8_192),
        };
        let updated = mark_success(&handler, &run.id, &snap).await.unwrap();

        assert_eq!(updated.status, "success");
        assert_eq!(updated.snapshot_id.as_deref(), Some("abcdef1234"));
        assert_eq!(updated.bytes_processed, Some(123_456));
        assert_eq!(updated.bytes_added, Some(8_192));
        assert!(updated.finished_at.is_some());
    }

    #[tokio::test]
    async fn mark_failure_records_reason() {
        let (handler, _td) = make_handler().await;
        let cfg = fake_cfg_with_one_job();
        let run = register_run(&handler, &cfg, "documents", "dashboard")
            .await
            .unwrap();

        let updated = mark_failure(&handler, &run.id, "disk full").await.unwrap();
        assert_eq!(updated.status, "failed");
        assert_eq!(updated.failure_reason.as_deref(), Some("disk full"));
        assert!(updated.finished_at.is_some());
    }

    #[tokio::test]
    async fn mark_success_on_unknown_run_errors() {
        let (handler, _td) = make_handler().await;
        let snap = SnapshotInfo {
            id: "nope".into(),
            time: "now".into(),
            paths: vec![],
            hostname: "h".into(),
            tags: vec![],
            total_bytes_processed: None,
            data_added: None,
        };
        let err = mark_success(&handler, "no-such-id", &snap)
            .await
            .expect_err("missing run should fail");
        assert!(err.contains("not found"));
    }
}
