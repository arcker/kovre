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
}
