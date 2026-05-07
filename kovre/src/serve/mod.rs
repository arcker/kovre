//! Embedded dashboard server (`kovre serve`).
//!
//! Brings up a Lithair server bound to the configured address with the
//! built-in `/health`, `/ready`, `/info` endpoints, the dashboard models
//! (currently `JobRun`) under `/api/*`, the trigger route
//! `POST /api/jobs/:name/run`, and (with `--debug`) the admin panel at
//! `/_admin/*`. The kovre.yaml ↔ runtime sync and the SvelteKit frontend
//! land in subsequent steps.

pub mod models;
pub mod runs;
pub mod sync;

use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::Full;
use kovre_core::config::Config;
use lithair_core::http::DeclarativeHttpHandler;
use lithair_core::LithairServer;
use tracing::info;

use crate::cli::ServeArgs;
use crate::serve::models::{JobRun, Snapshot};
use crate::serve::runs::{trigger_job_run, TriggerError};
use crate::serve::sync::sync_snapshots;

/// Entry point dispatched from `main::run` on `Command::Serve`.
///
/// Builds a multi-threaded Tokio runtime locally rather than wrapping the
/// whole binary in `#[tokio::main]`: the CLI subcommands (`run`, `list-jobs`,
/// …) stay synchronous and pay no runtime startup cost. Only `serve` needs
/// async, and only `serve` builds a runtime.
pub fn run(cfg: &Config, args: ServeArgs) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating Tokio runtime for `kovre serve`")?;

    // `<agent.data_dir>/lithair/` holds Lithair's event-sourced state.
    // Step 5 will surface this as `agent.dashboard.raftlog_dir` so it can
    // be overridden independently of `data_dir`; for now we derive it.
    let lithair_dir = cfg.agent.data_dir.join("lithair");
    let job_runs_path = lithair_dir.join("job_runs");
    let job_runs_path_str = job_runs_path.to_string_lossy().to_string();
    let snapshots_path = lithair_dir.join("snapshots");
    let snapshots_path_str = snapshots_path.to_string_lossy().to_string();
    let cfg_arc: Arc<Config> = Arc::new(cfg.clone());

    rt.block_on(async move {
        info!(
            bind = %args.bind,
            port = args.port,
            debug = args.debug,
            data_dir = %lithair_dir.display(),
            "starting kovre dashboard"
        );

        // Build the handlers explicitly so the same instance backs both
        // the auto-generated CRUD endpoints (via `with_handler`) and the
        // custom routes (via `with_route`).
        let job_runs: Arc<DeclarativeHttpHandler<JobRun>> = Arc::new(
            DeclarativeHttpHandler::<JobRun>::new_with_replay(&job_runs_path_str)
                .await
                .map_err(|e| anyhow::anyhow!("initializing JobRun event store: {e}"))?,
        );
        let snapshots: Arc<DeclarativeHttpHandler<Snapshot>> = Arc::new(
            DeclarativeHttpHandler::<Snapshot>::new_with_replay(&snapshots_path_str)
                .await
                .map_err(|e| anyhow::anyhow!("initializing Snapshot event store: {e}"))?,
        );

        // Materialize snapshots from rustic into the projection at boot.
        // Failures per-repo are logged but do not abort startup.
        let synced = sync_snapshots(&snapshots, &cfg_arc).await;
        info!(snapshots = synced, "initial snapshot sync completed");

        let mut server = LithairServer::new()
            .with_host(args.bind.to_string())
            .with_port(args.port)
            .with_handler(Arc::clone(&job_runs), "/api/job_runs")
            .with_handler(Arc::clone(&snapshots), "/api/snapshots");

        // POST /api/jobs/:name/run — see `serve::runs::trigger_job_run`.
        let runs_for_route = Arc::clone(&job_runs);
        let cfg_for_route = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::POST,
            "/api/jobs/*/run",
            move |req| {
                let runs = Arc::clone(&runs_for_route);
                let cfg = Arc::clone(&cfg_for_route);
                Box::pin(async move { handle_trigger(req, runs, cfg).await })
            },
        );

        // GET /api/jobs — read-only projection of kovre.yaml's `jobs:` block.
        // We expose it as a list endpoint (no individual /api/jobs/:name route)
        // because the frontend can filter client-side and we keep the API
        // surface tight.
        let cfg_for_jobs = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::GET,
            "/api/jobs",
            move |_req| {
                let cfg = Arc::clone(&cfg_for_jobs);
                Box::pin(async move { Ok(handle_list_jobs(cfg)) })
            },
        );

        // POST /api/sync — refresh the Snapshot projection on demand.
        // The boot-time sync only runs once; this lets the dashboard
        // pull in snapshots created out-of-band (e.g. via the CLI) without
        // restarting the server.
        let snapshots_for_sync = Arc::clone(&snapshots);
        let cfg_for_sync = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::POST,
            "/api/sync",
            move |_req| {
                let snapshots = Arc::clone(&snapshots_for_sync);
                let cfg = Arc::clone(&cfg_for_sync);
                Box::pin(async move {
                    let synced = sync_snapshots(&snapshots, &cfg).await;
                    Ok(json_response(
                        hyper::StatusCode::OK,
                        serde_json::json!({"synced": synced}),
                    ))
                })
            },
        );

        if args.debug {
            server = server.with_admin_panel(true);
        }

        server
            .serve()
            .await
            .context("Lithair server terminated with an error")
    })
}

/// HTTP wrapper around `runs::trigger_job_run`.
///
/// Extracts `:name` from `/api/jobs/<name>/run`, calls the policy layer,
/// and maps the outcome to:
///   - 202 Accepted with `{"id":"<uuid>"}` on success,
///   - 404 Not Found when the job is unknown,
///   - 409 Conflict when another run is in progress (returns the existing run id),
///   - 500 Internal Server Error on persistence failure.
async fn handle_trigger(
    req: hyper::Request<hyper::body::Incoming>,
    handler: Arc<DeclarativeHttpHandler<JobRun>>,
    cfg: Arc<Config>,
) -> anyhow::Result<hyper::Response<Full<Bytes>>> {
    let path = req.uri().path().to_string();
    let job_name = match extract_job_name(&path) {
        Some(name) => name,
        None => {
            return Ok(json_response(
                hyper::StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "could not parse job name from path"}),
            ));
        }
    };

    match trigger_job_run(handler, cfg, job_name, "dashboard".into()).await {
        Ok(run_id) => Ok(json_response(
            hyper::StatusCode::ACCEPTED,
            serde_json::json!({"id": run_id}),
        )),
        Err(TriggerError::UnknownJob { job }) => Ok(json_response(
            hyper::StatusCode::NOT_FOUND,
            serde_json::json!({"error": "unknown_job", "job": job}),
        )),
        Err(TriggerError::AlreadyRunning { run_id }) => Ok(json_response(
            hyper::StatusCode::CONFLICT,
            serde_json::json!({"error": "already_running", "run_id": run_id}),
        )),
        Err(TriggerError::Persistence { reason }) => Ok(json_response(
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "persistence", "reason": reason}),
        )),
    }
}

/// Pull the `<name>` segment out of `/api/jobs/<name>/run`. Returns
/// `None` if the path does not match this exact shape (defensive — the
/// router should already have filtered).
fn extract_job_name(path: &str) -> Option<String> {
    let stripped = path.strip_prefix("/api/jobs/")?;
    let (name, rest) = stripped.split_once('/')?;
    if rest != "run" || name.is_empty() {
        return None;
    }
    // URL-decode in case the job name contains characters that needed
    // encoding (spaces, accents, etc.). YAML allows them, the URL must
    // round-trip cleanly.
    Some(percent_decode(name))
}

/// Tiny percent-decoder limited to the set we actually expect in job
/// names. Adding the `percent-encoding` crate just for this would be
/// overkill given how constrained `kovre.yaml` job names are.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) =
                (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
            {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn json_response(status: hyper::StatusCode, body: serde_json::Value) -> hyper::Response<Full<Bytes>> {
    let bytes = Bytes::from(body.to_string());
    hyper::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(bytes))
        .expect("static headers + valid status never fails")
}

/// Project `kovre.yaml`'s `jobs:` block to a JSON array, attaching the
/// job name (which is the IndexMap key, not a struct field).
///
/// This route is read-only on purpose: jobs and repositories live in
/// the YAML and are the user's source of truth. The dashboard can read
/// them but cannot mutate them.
fn handle_list_jobs(cfg: Arc<Config>) -> hyper::Response<Full<Bytes>> {
    let body: Vec<serde_json::Value> = cfg
        .jobs
        .iter()
        .map(|(name, job)| {
            let mut value = serde_json::to_value(job).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
                obj.insert("name".into(), serde_json::Value::String(name.clone()));
            }
            value
        })
        .collect();
    json_response(hyper::StatusCode::OK, serde_json::Value::Array(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_job_name_handles_simple_name() {
        assert_eq!(
            extract_job_name("/api/jobs/documents/run"),
            Some("documents".into())
        );
    }

    #[test]
    fn extract_job_name_handles_percent_encoded_name() {
        assert_eq!(
            extract_job_name("/api/jobs/my%20job/run"),
            Some("my job".into())
        );
    }

    #[test]
    fn extract_job_name_rejects_wrong_suffix() {
        assert!(extract_job_name("/api/jobs/documents/list").is_none());
    }

    #[test]
    fn extract_job_name_rejects_missing_name() {
        assert!(extract_job_name("/api/jobs//run").is_none());
    }

    #[test]
    fn extract_job_name_rejects_unrelated_path() {
        assert!(extract_job_name("/health").is_none());
    }
}
