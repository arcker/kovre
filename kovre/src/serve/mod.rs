//! Embedded dashboard server (`kovre serve`).
//!
//! Brings up a Lithair server bound to the configured address with the
//! built-in `/health`, `/ready`, `/info` endpoints, the dashboard models
//! (currently `JobRun`) under `/api/*`, the trigger route
//! `POST /api/jobs/:name/run`, and (with `--debug`) the admin panel at
//! `/_admin/*`. The kovre.yaml ↔ runtime sync and the SvelteKit frontend
//! land in subsequent steps.
//!
//! TODO(lithair#59): direct deps on `bytes`, `http`, `http-body-util`,
//! `hyper` exist only to type the closures passed to `with_route`. Drop
//! them as soon as Lithair re-exports `RouteRequest` / `RouteResponse`
//! type aliases or ships a `Box::pin`-free closure helper.

pub mod frontend;
pub mod models;
pub mod runs;
pub mod sync;

use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use bytes::Bytes;
use http_body_util::Full;
use kovre_core::config::Config;
use lithair_core::app::response;
use lithair_core::http::query;
use lithair_core::http::DeclarativeHttpHandler;
use lithair_core::LithairServer;
use tracing::info;

/// Shared, swappable handle on the current `Config`. Phase 3 introduces
/// it so `PUT /api/config` can replace the running config without a
/// server restart. Reads are wait-free (`load_full()` returns a fresh
/// `Arc<Config>`); writes are atomic via `store(Arc::new(new))`.
pub type ConfigHandle = Arc<ArcSwap<Config>>;

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
    let cfg_arc: ConfigHandle = Arc::new(ArcSwap::from_pointee(cfg.clone()));

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
        let initial_cfg = cfg_arc.load_full();
        let synced = sync_snapshots(&snapshots, &initial_cfg).await;
        info!(snapshots = synced, "initial snapshot sync completed");

        let mut server = LithairServer::new()
            .with_host(args.bind.to_string())
            .with_port(args.port)
            .with_handler(Arc::clone(&job_runs), "/api/job_runs")
            .with_handler(Arc::clone(&snapshots), "/api/snapshots");

        // POST /api/jobs/:name/run — see `serve::runs::trigger_job_run`.
        // Each route closure captures the `ConfigHandle` (an
        // `Arc<ArcSwap<Config>>`) and snapshots it on every request via
        // `load_full()`, which yields the current `Arc<Config>` without
        // any blocking. This is what makes `PUT /api/config` able to swap
        // the config at run time and have subsequent requests pick it up.
        let runs_for_route = Arc::clone(&job_runs);
        let cfg_for_route: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::POST,
            "/api/jobs/*/run",
            move |req| {
                let runs = Arc::clone(&runs_for_route);
                let cfg = cfg_for_route.load_full();
                Box::pin(async move { handle_trigger(req, runs, cfg).await })
            },
        );

        // GET /api/jobs — read-only projection of kovre.yaml's `jobs:` block.
        // We expose it as a list endpoint (no individual /api/jobs/:name route)
        // because the frontend can filter client-side and we keep the API
        // surface tight.
        let cfg_for_jobs: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::GET,
            "/api/jobs",
            move |_req| {
                let cfg = cfg_for_jobs.load_full();
                Box::pin(async move { Ok(handle_list_jobs(cfg)) })
            },
        );

        // GET /api/config — current parsed Config plus a YAML serialization
        // of the same in-memory state. Phase 3's read path for the wizard
        // and the /config view.
        let cfg_for_get_config: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::GET,
            "/api/config",
            move |_req| {
                let cfg = cfg_for_get_config.load_full();
                Box::pin(async move { Ok(handle_get_config(cfg)) })
            },
        );

        // GET /api/templates — static catalog of the 4 builtin templates
        // (documents, dev-repos, steam-saves, custom) with their option
        // schema. Drives the gallery on /templates.
        server = server.with_route(
            http::Method::GET,
            "/api/templates",
            |_req| Box::pin(async move { Ok(handle_list_templates()) }),
        );

        // GET /api/fs?path=<dir> — list the direct subdirectories of
        // `<dir>`. Backend for the directory autocomplete in the wizard.
        server = server.with_route(
            http::Method::GET,
            "/api/fs",
            |req| Box::pin(async move { Ok(handle_list_fs(req)) }),
        );

        // POST /api/sync — refresh the Snapshot projection on demand.
        // The boot-time sync only runs once; this lets the dashboard
        // pull in snapshots created out-of-band (e.g. via the CLI) without
        // restarting the server.
        let snapshots_for_sync = Arc::clone(&snapshots);
        let cfg_for_sync: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route(
            http::Method::POST,
            "/api/sync",
            move |_req| {
                let snapshots = Arc::clone(&snapshots_for_sync);
                let cfg = cfg_for_sync.load_full();
                Box::pin(async move {
                    let synced = sync_snapshots(&snapshots, &cfg).await;
                    Ok(response::json_value(
                        hyper::StatusCode::OK,
                        &serde_json::json!({"synced": synced}),
                    ))
                })
            },
        );

        // Serve the embedded SvelteKit frontend.
        //
        // GET /              -> index.html (SPA shell)
        // GET /_app/**       -> hashed assets (JS, CSS, .wasm)
        // any unknown GET    -> SPA shell (handled by the not-found handler
        //                       below — SvelteKit owns client-side routing)
        // unknown /api/*     -> JSON 404 (also via not-found handler so we
        //                       never accidentally serve HTML to an API client)
        server = server.with_route(
            http::Method::GET,
            "/",
            |_req| {
                Box::pin(async move {
                    Ok(frontend::spa_shell()
                        .unwrap_or_else(frontend::asset_not_found))
                })
            },
        );
        server = server.with_route(
            http::Method::GET,
            "/_app/**",
            |req| {
                Box::pin(async move {
                    let path = req.uri().path().trim_start_matches('/').to_string();
                    Ok(frontend::asset_response(&path)
                        .unwrap_or_else(frontend::asset_not_found))
                })
            },
        );

        server = server.with_not_found_handler(|req| {
            Box::pin(async move {
                let path = req.uri().path().to_string();
                if path.starts_with("/api/")
                    || path == "/health"
                    || path == "/ready"
                    || path == "/info"
                {
                    return Ok(response::json_value(
                        hyper::StatusCode::NOT_FOUND,
                        &serde_json::json!({"error": "not_found", "path": path}),
                    ));
                }
                // Non-API path with no other handler — serve the SPA shell
                // and let SvelteKit's client router resolve the URL.
                Ok(frontend::spa_shell().unwrap_or_else(frontend::asset_not_found))
            })
        });

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
            return Ok(response::json_value(
                hyper::StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "could not parse job name from path"}),
            ));
        }
    };

    match trigger_job_run(handler, cfg, job_name, "dashboard".into()).await {
        Ok(run_id) => Ok(response::json_value(
            hyper::StatusCode::ACCEPTED,
            &serde_json::json!({"id": run_id}),
        )),
        Err(TriggerError::UnknownJob { job }) => Ok(response::json_value(
            hyper::StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "unknown_job", "job": job}),
        )),
        Err(TriggerError::AlreadyRunning { run_id }) => Ok(response::json_value(
            hyper::StatusCode::CONFLICT,
            &serde_json::json!({"error": "already_running", "run_id": run_id}),
        )),
        Err(TriggerError::Persistence { reason }) => Ok(response::json_value(
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": "persistence", "reason": reason}),
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
    Some(query::percent_decode(name))
}

/// `GET /api/config` — returns the current in-memory config in two
/// shapes: a YAML serialization (so the dashboard can show / download
/// it) and a parsed JSON tree (so forms can populate without re-
/// parsing). Both reflect the same `Arc<Config>` snapshot.
///
/// The YAML emitted here is a re-serialization of the parsed Config —
/// it does not preserve user comments or original key ordering on the
/// disk file. This is documented behavior: the dashboard's view of the
/// config is its in-memory model, not the raw file.
fn handle_get_config(cfg: Arc<Config>) -> hyper::Response<Full<Bytes>> {
    let yaml = match serde_yaml::to_string(&*cfg) {
        Ok(s) => s,
        Err(e) => {
            return response::json_value(
                hyper::StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({"error": "yaml_serialize", "reason": e.to_string()}),
            );
        }
    };
    let parsed = serde_json::to_value(&*cfg).unwrap_or(serde_json::Value::Null);
    response::json_value(
        hyper::StatusCode::OK,
        &serde_json::json!({"yaml": yaml, "parsed": parsed}),
    )
}

/// `GET /api/templates` — hard-coded catalog of the 4 templates the
/// dashboard's wizard knows how to instantiate. The schema is light:
/// each `option` is a `(key, type, label, required)` tuple the
/// frontend uses to render a form field. `directory` and
/// `directory_list` types tell the UI to use the autocomplete picker
/// backed by `GET /api/fs`.
fn handle_list_templates() -> hyper::Response<Full<Bytes>> {
    let body = serde_json::json!([
        {
            "name": "documents",
            "icon": "📄",
            "description": "Backup the user's Documents, Desktop and Pictures folders.",
            "options": []
        },
        {
            "name": "dev-repos",
            "icon": "⚙️",
            "description": "Find every git repository under a scan root and back them up.",
            "options": [
                {"key": "scan_root", "type": "directory", "label": "Scan root", "required": false}
            ]
        },
        {
            "name": "steam-saves",
            "icon": "🎮",
            "description": "Detect Steam via the registry and back up game save folders matched against the Ludusavi manifest.",
            "options": []
        },
        {
            "name": "custom",
            "icon": "📂",
            "description": "Pick one or more folders to back up, with optional exclude patterns.",
            "options": [
                {"key": "paths", "type": "directory_list", "label": "Folders to back up", "required": true},
                {"key": "excludes", "type": "string_list", "label": "Exclude patterns (glob)", "required": false}
            ]
        }
    ]);
    response::json_value(hyper::StatusCode::OK, &body)
}

/// `GET /api/fs?path=<dir>` — list direct subdirectories of `<dir>`.
///
/// Returns a flat structure: `{path, entries: [{name, is_dir}]}`. Files
/// are filtered out — the picker is a folder picker. `path` must:
///   - be present (no default; an empty path is rejected),
///   - not contain a `..` component (defensive guard against path
///     traversal even though the dashboard binds 127.0.0.1 by default),
///   - exist on disk.
///
/// Split into an HTTP wrapper and a pure helper so the policy is unit
/// testable without constructing a hyper Request.
fn handle_list_fs(req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<Full<Bytes>> {
    list_fs_for_query(req.uri().query().unwrap_or(""))
}

fn list_fs_for_query(query: &str) -> hyper::Response<Full<Bytes>> {
    let raw_path = match query::param(query, "path") {
        Some(p) if !p.is_empty() => p,
        _ => {
            return response::json_value(
                hyper::StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "missing_path", "hint": "GET /api/fs?path=<dir>"}),
            );
        }
    };

    if raw_path.split(['/', '\\']).any(|seg| seg == "..") {
        return response::json_value(
            hyper::StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "path_traversal", "path": raw_path}),
        );
    }

    let path = std::path::PathBuf::from(&raw_path);
    if !path.exists() {
        return response::json_value(
            hyper::StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "path_not_found", "path": raw_path}),
        );
    }
    if !path.is_dir() {
        return response::json_value(
            hyper::StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "not_a_directory", "path": raw_path}),
        );
    }

    let entries: Vec<serde_json::Value> = match std::fs::read_dir(&path) {
        Ok(it) => it
            .filter_map(|res| res.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .map(|s| serde_json::json!({"name": s, "is_dir": true}))
            })
            .collect(),
        Err(e) => {
            return response::json_value(
                hyper::StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({"error": "io", "reason": e.to_string(), "path": raw_path}),
            );
        }
    };

    response::json_value(
        hyper::StatusCode::OK,
        &serde_json::json!({"path": raw_path, "entries": entries}),
    )
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
    response::json_value(hyper::StatusCode::OK, &serde_json::Value::Array(body))
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

    #[test]
    fn handle_list_templates_returns_the_four_known_templates() {
        let resp = handle_list_templates();
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        let body_bytes = response_body(resp);
        let arr: Vec<serde_json::Value> = serde_json::from_slice(&body_bytes).unwrap();
        let names: Vec<String> = arr
            .iter()
            .map(|v| v["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            names,
            vec!["documents", "dev-repos", "steam-saves", "custom"]
        );
    }

    #[test]
    fn handle_get_config_round_trips_yaml_and_parsed() {
        let cfg = sample_cfg();
        let resp = handle_get_config(Arc::new(cfg));
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&response_body(resp)).unwrap();
        assert!(body["yaml"].is_string());
        assert!(body["parsed"].is_object());
        assert_eq!(body["parsed"]["jobs"]["documents"]["repository"], "test");

        // The yaml field should re-parse back to the same Config.
        let yaml = body["yaml"].as_str().unwrap();
        let reparsed = Config::from_str(yaml, std::path::Path::new("test.yaml")).unwrap();
        assert!(reparsed.jobs.contains_key("documents"));
    }

    #[test]
    fn list_fs_lists_subdirectories() {
        let tempdir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tempdir.path().join("alpha")).unwrap();
        std::fs::create_dir(tempdir.path().join("beta")).unwrap();
        std::fs::write(tempdir.path().join("file.txt"), b"hi").unwrap();

        let resp = list_fs_for_query(&fs_query(tempdir.path().to_str().unwrap()));
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&response_body(resp)).unwrap();
        let names: Vec<String> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
        assert!(!names.contains(&"file.txt".to_string()), "files must be filtered out");
    }

    #[test]
    fn list_fs_rejects_missing_path() {
        let resp = list_fs_for_query("");
        assert_eq!(resp.status(), hyper::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn list_fs_rejects_path_traversal() {
        let resp = list_fs_for_query(&fs_query("C:\\Users\\..\\Windows"));
        assert_eq!(resp.status(), hyper::StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_slice(&response_body(resp)).unwrap();
        assert_eq!(body["error"], "path_traversal");
    }

    #[test]
    fn list_fs_returns_404_for_missing_dir() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let missing = tempdir.path().join("does-not-exist");
        let resp = list_fs_for_query(&fs_query(missing.to_str().unwrap()));
        assert_eq!(resp.status(), hyper::StatusCode::NOT_FOUND);
    }

    // ---- helpers ----

    fn sample_cfg() -> Config {
        use indexmap::IndexMap;
        use kovre_core::config::{Agent, Job, Repository};
        use std::path::PathBuf;
        let mut repositories = IndexMap::new();
        repositories.insert(
            "test".into(),
            Repository {
                path: PathBuf::from(r"C:\nope"),
                password_file: PathBuf::from(r"C:\nope.key"),
            },
        );
        let mut jobs = IndexMap::new();
        jobs.insert(
            "documents".into(),
            Job {
                repository: "test".into(),
                template: Some("documents".into()),
                template_options: None,
                paths: None,
                excludes: None,
                retention: None,
            },
        );
        Config {
            agent: Agent {
                data_dir: PathBuf::from(r"C:\ProgramData\Kovre"),
                log_level: "info".into(),
            },
            repositories,
            jobs,
        }
    }

    fn fs_query(path: &str) -> String {
        let mut encoded = String::new();
        for byte in path.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    encoded.push(byte as char);
                }
                other => encoded.push_str(&format!("%{other:02X}")),
            }
        }
        format!("path={encoded}")
    }

    fn response_body(resp: hyper::Response<Full<Bytes>>) -> Vec<u8> {
        use http_body_util::BodyExt;
        let (_parts, body) = resp.into_parts();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        rt.block_on(async move { body.collect().await.unwrap().to_bytes().to_vec() })
    }
}
