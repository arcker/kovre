//! Lithair models persisted by the dashboard server.
//!
//! `Repository` and `Job` deliberately do NOT live here — they are read
//! from `kovre.yaml` at startup and remain immutable runtime config.
//! Only *runtime state* (run history, snapshot cache, key/value settings)
//! flows through Lithair's event-sourced storage.

use lithair_core::DeclarativeModel;
use serde::{Deserialize, Serialize};

/// One execution of a backup job.
///
/// Field types are intentionally primitive (no domain enums) so the
/// `DeclarativeModel` derive in `lithair-core 0.2.x` accepts them
/// without needing custom `HttpExposable`/`LifecycleAware` impls.
/// Status and trigger are validated as strings: `"running"`,
/// `"success"`, `"failed"` for status; `"cli"`, `"dashboard"`,
/// `"scheduled"` for trigger. Wider validation lives in the route
/// layer when those states are written by code paths inside kovre.
///
/// Timestamps are RFC 3339 strings rather than `jiff::Zoned` to
/// keep the model serializable through any serde format Lithair
/// chooses internally and to avoid leaking the `jiff` version
/// boundary into the API surface.
#[derive(Debug, Clone, Serialize, Deserialize, DeclarativeModel)]
pub struct JobRun {
    /// UUID v4, formatted as a hyphenated lowercase string.
    #[http(expose)]
    #[lifecycle(immutable)]
    #[db(unique)]
    pub id: String,

    /// Name of the job (matches a key in `kovre.yaml::jobs`).
    #[http(expose)]
    #[lifecycle(immutable)]
    pub job_name: String,

    /// RFC 3339 timestamp of the moment the run started.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub started_at: String,

    /// RFC 3339 timestamp; `None` while the run is still in `running` state.
    #[http(expose)]
    pub finished_at: Option<String>,

    /// One of `"running"`, `"success"`, `"failed"`.
    #[http(expose)]
    pub status: String,

    /// Human-readable reason; only set when `status == "failed"`.
    #[http(expose)]
    pub failure_reason: Option<String>,

    /// rustic snapshot id when the run produced one (success path).
    #[http(expose)]
    pub snapshot_id: Option<String>,

    /// Total bytes the backup walked through (snapshot summary).
    #[http(expose)]
    pub bytes_processed: Option<u64>,

    /// Net new bytes added to the repository (deduplication net).
    #[http(expose)]
    pub bytes_added: Option<u64>,

    /// One of `"cli"`, `"dashboard"`, `"scheduled"`.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub trigger: String,
}

/// One execution of a restore — the inverse of `JobRun`. Same shape
/// modulo the fields that don't apply (no snapshot produced, no
/// bytes_added: a restore doesn't grow the repository). Carries
/// `dest_dir` so the dashboard can show "where did this go?" in the
/// run history.
///
/// At most one restore can be `running` per `(job_name, dest_dir)`
/// pair — `register_restore_run` enforces that to prevent two
/// concurrent restores stomping on the same destination.
#[derive(Debug, Clone, Serialize, Deserialize, DeclarativeModel)]
pub struct RestoreRun {
    /// UUID v4, formatted as a hyphenated lowercase string.
    #[http(expose)]
    #[lifecycle(immutable)]
    #[db(unique)]
    pub id: String,

    /// Name of the job whose backed-up content is being restored.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub job_name: String,

    /// Destination directory the restore writes into.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub dest_dir: String,

    /// RFC 3339 timestamp of the moment the run started.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub started_at: String,

    /// RFC 3339 timestamp; `None` while the run is still in `running` state.
    #[http(expose)]
    pub finished_at: Option<String>,

    /// One of `"running"`, `"success"`, `"failed"`.
    #[http(expose)]
    pub status: String,

    /// Human-readable reason; only set when `status == "failed"`.
    #[http(expose)]
    pub failure_reason: Option<String>,

    /// One of `"cli"`, `"dashboard"`.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub trigger: String,
}

/// Cached projection of a rustic snapshot, refreshed at server startup
/// and on demand by the sync layer (`serve::sync`).
///
/// kovre never authoritatively *creates* a `Snapshot` from the dashboard —
/// the source of truth lives in the rustic repository. POST/PUT/DELETE
/// on `/api/snapshots` therefore only mutate the projection, never the
/// underlying repo.
#[derive(Debug, Clone, Serialize, Deserialize, DeclarativeModel)]
pub struct Snapshot {
    /// rustic snapshot id (full hex string).
    #[http(expose)]
    #[lifecycle(immutable)]
    #[db(unique)]
    pub id: String,

    /// Name of the kovre job that produced this snapshot, derived from
    /// the `kovre-job:<name>` tag attached at backup time.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub job_name: String,

    /// RFC 3339 timestamp.
    #[http(expose)]
    #[lifecycle(immutable)]
    pub time: String,

    /// Source paths captured by the snapshot (string-formatted).
    #[http(expose)]
    pub paths: Vec<String>,

    /// Hostname recorded by rustic at backup time.
    #[http(expose)]
    pub hostname: String,

    /// Snapshot summary: `total_bytes_processed`. `None` if rustic did
    /// not record a summary (older snapshots).
    #[http(expose)]
    pub bytes_total: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lithair_core::testing::TestHandler;

    fn sample_run(id: &str) -> JobRun {
        JobRun {
            id: id.into(),
            job_name: "documents".into(),
            started_at: "2026-05-05T12:00:00Z".into(),
            finished_at: None,
            status: "running".into(),
            failure_reason: None,
            snapshot_id: None,
            bytes_processed: None,
            bytes_added: None,
            trigger: "dashboard".into(),
        }
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let h = TestHandler::<JobRun>::new().await.expect("TestHandler::new");
        let run = sample_run("11111111-1111-4111-8111-111111111111");

        h.create(run.clone()).await.expect("create");

        let got = h.get(&run.id).await.expect("get returned None");
        assert_eq!(got.id, run.id);
        assert_eq!(got.job_name, "documents");
        assert_eq!(got.status, "running");
        assert_eq!(got.trigger, "dashboard");
        assert!(got.finished_at.is_none());
    }

    #[tokio::test]
    async fn list_returns_inserted_runs() {
        let h = TestHandler::<JobRun>::new().await.unwrap();
        h.create(sample_run("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"))
            .await
            .unwrap();
        h.create(sample_run("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"))
            .await
            .unwrap();

        let all = h.list().await;
        assert_eq!(all.len(), 2);
        assert_eq!(h.count().await, 2);
    }

    #[tokio::test]
    async fn duplicate_id_is_rejected() {
        let h = TestHandler::<JobRun>::new().await.unwrap();
        let run = sample_run("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
        h.create(run.clone()).await.unwrap();

        let err = h.create(run).await.expect_err("duplicate id should fail");
        assert!(
            err.to_lowercase().contains("unique") || err.to_lowercase().contains("exists"),
            "unexpected error message: {err}"
        );
    }

    fn sample_restore(id: &str) -> RestoreRun {
        RestoreRun {
            id: id.into(),
            job_name: "documents".into(),
            dest_dir: r"C:\Users\me\kovre-restore\documents\2026-05-20".into(),
            started_at: "2026-05-20T08:00:00Z".into(),
            finished_at: None,
            status: "running".into(),
            failure_reason: None,
            trigger: "dashboard".into(),
        }
    }

    #[tokio::test]
    async fn restore_run_create_and_get_round_trip() {
        let h = TestHandler::<RestoreRun>::new().await.unwrap();
        let run = sample_restore("11111111-1111-4111-8111-aaaaaaaaaaaa");
        h.create(run.clone()).await.unwrap();
        let got = h.get(&run.id).await.expect("get returned None");
        assert_eq!(got.dest_dir, run.dest_dir);
        assert_eq!(got.status, "running");
        assert!(got.finished_at.is_none());
    }

    #[tokio::test]
    async fn restore_run_duplicate_id_is_rejected() {
        let h = TestHandler::<RestoreRun>::new().await.unwrap();
        let run = sample_restore("22222222-2222-4222-8222-bbbbbbbbbbbb");
        h.create(run.clone()).await.unwrap();
        let err = h.create(run).await.expect_err("duplicate id should fail");
        assert!(
            err.to_lowercase().contains("unique") || err.to_lowercase().contains("exists")
        );
    }
}
