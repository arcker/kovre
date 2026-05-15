//! Embedded dashboard server (`kovre serve`).
//!
//! Brings up a Lithair server bound to the configured address with the
//! built-in `/health`, `/ready`, `/info` endpoints, the dashboard models
//! (currently `JobRun`) under `/api/*`, the trigger route
//! `POST /api/jobs/:name/run`, and (with `--debug`) the admin panel at
//! `/_admin/*`. The kovre.yaml ↔ runtime sync and the SvelteKit frontend
//! land in subsequent steps.

pub mod frontend;
pub mod models;
pub mod runs;
pub mod sync;

use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use kovre_core::config::Config;
use lithair_core::app::{response, Method, RouteRequest, RouteResponse, StatusCode};
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
pub fn run(cfg: &Config, config_path: std::path::PathBuf, args: ServeArgs) -> Result<()> {
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
        server = server.with_route_async(Method::POST, "/api/jobs/*/run", move |req| {
            let runs = Arc::clone(&runs_for_route);
            let cfg = cfg_for_route.load_full();
            async move { handle_trigger(req, runs, cfg).await }
        });

        // GET /api/jobs — read-only projection of kovre.yaml's `jobs:` block.
        // We expose it as a list endpoint (no individual /api/jobs/:name route)
        // because the frontend can filter client-side and we keep the API
        // surface tight.
        let cfg_for_jobs: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(Method::GET, "/api/jobs", move |_req| {
            let cfg = cfg_for_jobs.load_full();
            async move { Ok(handle_list_jobs(cfg)) }
        });

        // GET /api/config — current parsed Config plus a YAML serialization
        // of the same in-memory state. Phase 3's read path for the wizard
        // and the /config view.
        let cfg_for_get_config: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(Method::GET, "/api/config", move |_req| {
            let cfg = cfg_for_get_config.load_full();
            async move { Ok(handle_get_config(cfg)) }
        });

        // PUT /api/config — accepts a YAML body, validates it via
        // `Config::from_str`, writes the file atomically, and swaps
        // the in-memory `ArcSwap` so subsequent requests see the new
        // config without a server restart. On parse error the file is
        // never touched and the in-memory state stays put.
        let cfg_for_put_config: ConfigHandle = Arc::clone(&cfg_arc);
        let path_for_put_config = config_path.clone();
        server =
            server.with_route_async(Method::PUT, "/api/config", move |req| {
                let swap = Arc::clone(&cfg_for_put_config);
                let path = path_for_put_config.clone();
                async move { Ok(handle_put_config(req, swap, path).await) }
            });

        // GET /api/templates — static catalog of the 4 builtin templates
        // (documents, dev-repos, steam-saves, custom) with their option
        // schema. Drives the gallery on /templates.
        server = server.with_route_async(Method::GET, "/api/templates", |_req| async move {
            Ok(handle_list_templates())
        });

        // GET /api/fs?path=<dir> — list the direct subdirectories of
        // `<dir>`. Backend for the directory autocomplete in the wizard.
        server = server.with_route_async(Method::GET, "/api/fs", |req| async move {
            Ok(handle_list_fs(req))
        });

        // GET /api/fs/stat?path=<p> — file/directory existence and type.
        // Used by the repository wizard to warn (not block) when the
        // password_file path points at something that does not exist.
        server = server.with_route_async(Method::GET, "/api/fs/stat", |req| async move {
            Ok(handle_fs_stat(req))
        });

        // POST /api/repositories/init-password { path } — generates a
        // 32-byte random passphrase (hex-encoded), writes it atomically
        // to `<path>`. Refuses to overwrite an existing file so the
        // user never destroys a real passphrase by accident.
        server = server.with_route_async(
            Method::POST,
            "/api/repositories/init-password",
            |req| async move { Ok(handle_init_password(req).await) },
        );

        // POST /api/repositories/:name/init — materialize the rustic
        // repository on disk (creates the `config` file + keys + empty
        // index). Idempotent-style: returns 409 if the repo is already
        // initialized so the wizard can call this unconditionally and
        // surface "already initialized" without aborting.
        let cfg_for_init: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(
            Method::POST,
            "/api/repositories/*/init",
            move |req| {
                let cfg = cfg_for_init.load_full();
                async move { Ok(handle_init_repo(req, cfg).await) }
            },
        );

        // POST /api/repositories/:name/verify — run an integrity check.
        // For rustic this walks the metadata + index via rustic_core's
        // `check`; for mirror it's a no-op (files are native on disk).
        // Wrapped in `spawn_blocking` because the rustic path is sync
        // and can take a while on large repos.
        let cfg_for_verify: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(
            Method::POST,
            "/api/repositories/*/verify",
            move |req| {
                let cfg = cfg_for_verify.load_full();
                async move { Ok(handle_verify_repo(req, cfg).await) }
            },
        );

        // GET /api/repositories/status — per-repo `{initialized: bool}`.
        // Drives the conditional rendering of the "init" button on
        // /repositories: a repo whose `<path>/config` exists on disk
        // is hidden from the init affordance.
        let cfg_for_status: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(
            Method::GET,
            "/api/repositories/status",
            move |_req| {
                let cfg = cfg_for_status.load_full();
                async move { Ok(handle_repositories_status(&cfg)) }
            },
        );

        // POST /api/sync — refresh the Snapshot projection on demand.
        // The boot-time sync only runs once; this lets the dashboard
        // pull in snapshots created out-of-band (e.g. via the CLI) without
        // restarting the server.
        let snapshots_for_sync = Arc::clone(&snapshots);
        let cfg_for_sync: ConfigHandle = Arc::clone(&cfg_arc);
        server = server.with_route_async(Method::POST, "/api/sync", move |_req| {
            let snapshots = Arc::clone(&snapshots_for_sync);
            let cfg = cfg_for_sync.load_full();
            async move {
                let synced = sync_snapshots(&snapshots, &cfg).await;
                Ok(response::json_value(
                    StatusCode::OK,
                    &serde_json::json!({"synced": synced}),
                ))
            }
        });

        // Serve the embedded SvelteKit frontend.
        //
        // GET /              -> index.html (SPA shell)
        // GET /_app/**       -> hashed assets (JS, CSS, .wasm)
        // any unknown GET    -> SPA shell (handled by the not-found handler
        //                       below — SvelteKit owns client-side routing)
        // unknown /api/*     -> JSON 404 (also via not-found handler so we
        //                       never accidentally serve HTML to an API client)
        server = server.with_route_async(Method::GET, "/", |_req| async move {
            Ok(frontend::spa_shell().unwrap_or_else(frontend::asset_not_found))
        });
        server = server.with_route_async(Method::GET, "/_app/**", |req| async move {
            let path = req.uri().path().trim_start_matches('/').to_string();
            Ok(frontend::asset_response(&path).unwrap_or_else(frontend::asset_not_found))
        });

        server = server.with_not_found_handler_async(|req| async move {
            let path = req.uri().path().to_string();
            if path.starts_with("/api/")
                || path == "/health"
                || path == "/ready"
                || path == "/info"
            {
                return Ok(response::json_value(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({"error": "not_found", "path": path}),
                ));
            }
            // Non-API path with no other handler — serve the SPA shell
            // and let SvelteKit's client router resolve the URL.
            Ok(frontend::spa_shell().unwrap_or_else(frontend::asset_not_found))
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
    req: RouteRequest,
    handler: Arc<DeclarativeHttpHandler<JobRun>>,
    cfg: Arc<Config>,
) -> anyhow::Result<RouteResponse> {
    let path = req.uri().path().to_string();
    let job_name = match extract_job_name(&path) {
        Some(name) => name,
        None => {
            return Ok(response::json_value(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "could not parse job name from path"}),
            ));
        }
    };

    match trigger_job_run(handler, cfg, job_name, "dashboard".into()).await {
        Ok(run_id) => Ok(response::json_value(
            StatusCode::ACCEPTED,
            &serde_json::json!({"id": run_id}),
        )),
        Err(TriggerError::UnknownJob { job }) => Ok(response::json_value(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "unknown_job", "job": job}),
        )),
        Err(TriggerError::AlreadyRunning { run_id }) => Ok(response::json_value(
            StatusCode::CONFLICT,
            &serde_json::json!({"error": "already_running", "run_id": run_id}),
        )),
        Err(TriggerError::Persistence { reason }) => Ok(response::json_value(
            StatusCode::INTERNAL_SERVER_ERROR,
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
fn handle_get_config(cfg: Arc<Config>) -> RouteResponse {
    let (status, body) = get_config_data(&cfg);
    response::json_value(status, &body)
}

/// Pure policy: serialize the in-memory `Config` to JSON+YAML. Pulled
/// out of `handle_get_config` so unit tests assert directly on the
/// payload.
fn get_config_data(cfg: &Config) -> (StatusCode, serde_json::Value) {
    let yaml = match serde_yaml::to_string(cfg) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": "yaml_serialize", "reason": e.to_string()}),
            );
        }
    };
    let parsed = serde_json::to_value(cfg).unwrap_or(serde_json::Value::Null);
    (
        StatusCode::OK,
        serde_json::json!({"yaml": yaml, "parsed": parsed}),
    )
}

/// `PUT /api/config` — replace `kovre.yaml` with the request body,
/// reload the running config in-place.
///
/// Validates the YAML through `Config::from_str` before touching the
/// disk. On parse failure: the file is left untouched, the running
/// config is unchanged, and the response carries line/column from
/// `serde_yaml::Error::location()` so the dashboard can show a
/// precise error. On success: writes atomically (via `atomicwrites`,
/// rename-from-tmp pattern), then swaps the `ArcSwap` — every
/// subsequent request reads the new config without a server restart.
///
/// Body limit: 256 KiB. kovre.yaml never exceeds a few KB in practice;
/// the limit exists to reject obviously-wrong uploads up front rather
/// than allocate a multi-MB string.
async fn handle_put_config(
    req: RouteRequest,
    swap: ConfigHandle,
    config_path: std::path::PathBuf,
) -> RouteResponse {
    use lithair_core::app::request;

    const MAX_BODY: usize = 256 * 1024;
    let yaml = match request::read_body_with_limit(req, MAX_BODY).await {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                return response::json_value(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({"error": "non_utf8_body", "reason": e.to_string()}),
                );
            }
        },
        Err(e) => {
            return response::json_value(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "read_body", "reason": e.to_string()}),
            );
        }
    };

    let (status, body) = put_config_data(&yaml, &config_path, &swap);
    response::json_value(status, &body)
}

/// Pure policy: validate, write, swap. Split from the HTTP wrapper so
/// unit tests can drive it without spinning up Lithair.
fn put_config_data(
    yaml: &str,
    config_path: &std::path::Path,
    swap: &ConfigHandle,
) -> (StatusCode, serde_json::Value) {
    use kovre_core::config::ConfigError;

    // 1. Validate. Don't touch disk or memory if this fails.
    let new_cfg = match Config::from_str(yaml, config_path) {
        Ok(c) => c,
        Err(ConfigError::Parse { source, .. }) => {
            let location = source.location().map(|loc| {
                serde_json::json!({"line": loc.line(), "column": loc.column()})
            });
            return (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "yaml_parse",
                    "message": source.to_string(),
                    "location": location,
                }),
            );
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "config_validation",
                    "message": e.to_string(),
                }),
            );
        }
    };

    // 2. Persist atomically. `atomicwrites` writes to a sibling temp
    //    file then renames over the target, so a crash mid-write never
    //    leaves a truncated kovre.yaml on disk.
    use atomicwrites::{AllowOverwrite, AtomicFile};
    use std::io::Write;
    let af = AtomicFile::new(config_path, AllowOverwrite);
    if let Err(e) = af.write(|f| f.write_all(yaml.as_bytes())) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "io_write", "message": e.to_string()}),
        );
    }

    // 3. Live swap. Reads on subsequent requests pick this up via
    //    `cfg_handle.load_full()` (wait-free thanks to ArcSwap).
    swap.store(Arc::new(new_cfg.clone()));

    // 4. Return the new config, in the same shape as GET /api/config,
    //    so the dashboard can refresh its local view in one round-trip.
    let (_, body) = get_config_data(&new_cfg);
    (StatusCode::OK, body)
}

/// `GET /api/templates` — hard-coded catalog of the 4 templates the
/// dashboard's wizard knows how to instantiate. The schema is light:
/// each `option` is a `(key, type, label, required)` tuple the
/// frontend uses to render a form field. `directory` and
/// `directory_list` types tell the UI to use the autocomplete picker
/// backed by `GET /api/fs`.
fn handle_list_templates() -> RouteResponse {
    response::json_value(StatusCode::OK, &list_templates_data())
}

/// `GET /api/repositories/status` — report whether each repository
/// declared in `kovre.yaml` has been materialized on disk by rustic.
/// "Initialized" means a `config` file exists at `<repo.path>/config`,
/// which is the marker rustic itself uses to detect a live repo.
fn handle_repositories_status(cfg: &Config) -> RouteResponse {
    let mut map = serde_json::Map::new();
    for (name, repo) in &cfg.repositories {
        let config_file = repo.path.join("config");
        map.insert(
            name.clone(),
            serde_json::json!({ "initialized": config_file.is_file() }),
        );
    }
    response::json_value(StatusCode::OK, &serde_json::Value::Object(map))
}

/// `POST /api/repositories/:name/init` — run `kovre_core::backup::init_repo`
/// against the named repository. The rustic write is synchronous and
/// touches disk, so we wrap it in `spawn_blocking`.
async fn handle_init_repo(req: RouteRequest, cfg: Arc<Config>) -> RouteResponse {
    let path = req.uri().path().to_string();
    let name = match extract_repo_name_for_init(&path) {
        Some(n) => n,
        None => {
            return response::json_value(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "could not parse repository name from path"}),
            );
        }
    };

    let repo = match cfg.repositories.get(&name) {
        Some(r) => r.clone(),
        None => {
            return response::json_value(
                StatusCode::NOT_FOUND,
                &serde_json::json!({"error": "unknown_repository", "name": name}),
            );
        }
    };

    let result =
        tokio::task::spawn_blocking(move || kovre_core::backup::engine_for(&repo).init()).await;
    match result {
        Ok(Ok(())) => response::json_value(
            StatusCode::OK,
            &serde_json::json!({"initialized": name}),
        ),
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            // rustic refuses to re-init an existing repo. Surface that
            // as a 409 so the wizard can show "already initialized"
            // rather than a generic 500.
            let lower = msg.to_lowercase();
            let already = lower.contains("already") || lower.contains("config file already");
            response::json_value(
                if already { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST },
                &serde_json::json!({
                    "error": if already { "already_initialized" } else { "init_failed" },
                    "name": name,
                    "message": msg,
                }),
            )
        }
        Err(join_err) => response::json_value(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": "init_task_panicked", "reason": join_err.to_string()}),
        ),
    }
}

/// Pull `<name>` out of `/api/repositories/<name>/init`.
fn extract_repo_name_for_init(path: &str) -> Option<String> {
    extract_repo_name_for_action(path, "init")
}

/// Pull `<name>` out of `/api/repositories/<name>/verify`.
fn extract_repo_name_for_verify(path: &str) -> Option<String> {
    extract_repo_name_for_action(path, "verify")
}

fn extract_repo_name_for_action(path: &str, action: &str) -> Option<String> {
    let stripped = path.strip_prefix("/api/repositories/")?;
    let (name, rest) = stripped.split_once('/')?;
    if rest != action || name.is_empty() {
        return None;
    }
    Some(query::percent_decode(name))
}

/// `POST /api/repositories/:name/verify` — run an integrity check on
/// the named repository. Always returns 200 with a structured outcome
/// (`{ok: bool, messages: [..]}`); transport failures still surface as
/// 4xx/5xx with a `{error: ...}` body.
async fn handle_verify_repo(req: RouteRequest, cfg: Arc<Config>) -> RouteResponse {
    let path = req.uri().path().to_string();
    let name = match extract_repo_name_for_verify(&path) {
        Some(n) => n,
        None => {
            return response::json_value(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "could not parse repository name from path"}),
            );
        }
    };

    let repo = match cfg.repositories.get(&name) {
        Some(r) => r.clone(),
        None => {
            return response::json_value(
                StatusCode::NOT_FOUND,
                &serde_json::json!({"error": "unknown_repository", "name": name}),
            );
        }
    };

    let result =
        tokio::task::spawn_blocking(move || kovre_core::backup::engine_for(&repo).verify()).await;
    match result {
        Ok(Ok(outcome)) => response::json_value(
            StatusCode::OK,
            &serde_json::json!({
                "ok": outcome.ok,
                "messages": outcome.messages,
                "name": name,
            }),
        ),
        Ok(Err(e)) => response::json_value(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({
                "error": "verify_failed",
                "name": name,
                "message": format!("{e:#}"),
            }),
        ),
        Err(join_err) => response::json_value(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": "verify_task_panicked", "reason": join_err.to_string()}),
        ),
    }
}

/// `GET /api/fs/stat?path=<p>` — answer whether a path exists, and what
/// kind of entry sits there. Wraps `list_fs_stat_data`.
fn handle_fs_stat(req: RouteRequest) -> RouteResponse {
    let (status, body) = list_fs_stat_data(req.uri().query().unwrap_or(""));
    response::json_value(status, &body)
}

fn list_fs_stat_data(query: &str) -> (StatusCode, serde_json::Value) {
    let raw_path = match query::param(query, "path") {
        Some(p) if !p.is_empty() => p,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "missing_path"}),
            );
        }
    };
    if raw_path.split(['/', '\\']).any(|seg| seg == "..") {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "path_traversal", "path": raw_path}),
        );
    }
    let path = std::path::PathBuf::from(&raw_path);
    match std::fs::metadata(&path) {
        Ok(md) => (
            StatusCode::OK,
            serde_json::json!({
                "exists": true,
                "is_file": md.is_file(),
                "is_dir": md.is_dir(),
                "size": md.len(),
                "path": raw_path,
            }),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::OK,
            serde_json::json!({
                "exists": false,
                "is_file": false,
                "is_dir": false,
                "path": raw_path,
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "io", "reason": e.to_string(), "path": raw_path}),
        ),
    }
}

/// `POST /api/repositories/init-password { path }` — write a fresh
/// 32-byte (hex-encoded, 64 chars) random passphrase to the given
/// path. Refuses to overwrite an existing file: silently destroying
/// a real passphrase would orphan a rustic repository forever.
async fn handle_init_password(req: RouteRequest) -> RouteResponse {
    use lithair_core::app::request;

    #[derive(serde::Deserialize)]
    struct Body {
        path: String,
    }
    let body: Body = match request::read_body_json(req).await {
        Ok(b) => b,
        Err(e) => {
            return response::json_value(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "read_body", "reason": e.to_string()}),
            );
        }
    };

    let (status, payload) = init_password_data(&body.path);
    response::json_value(status, &payload)
}

fn init_password_data(raw_path: &str) -> (StatusCode, serde_json::Value) {
    if raw_path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "missing_path"}),
        );
    }
    if raw_path.split(['/', '\\']).any(|seg| seg == "..") {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "path_traversal", "path": raw_path}),
        );
    }

    let path = std::path::PathBuf::from(raw_path);
    if path.exists() {
        return (
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "file_exists",
                "path": raw_path,
                "hint": "refusing to overwrite; delete the file or pick a different path",
            }),
        );
    }

    let mut buf = [0u8; 32];
    if let Err(e) = getrandom::getrandom(&mut buf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "rand", "reason": e.to_string()}),
        );
    }
    let mut hex = String::with_capacity(64);
    for byte in buf {
        hex.push_str(&format!("{byte:02x}"));
    }

    // Ensure the parent directory exists. Lots of users will point at
    // `C:\ProgramData\Kovre\nas.key` on a fresh box where the directory
    // hasn't been created yet.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "mkdir", "reason": e.to_string(), "path": raw_path}),
                );
            }
        }
    }

    use atomicwrites::{AllowOverwrite, AtomicFile};
    use std::io::Write;
    let af = AtomicFile::new(&path, AllowOverwrite);
    if let Err(e) = af.write(|f| f.write_all(hex.as_bytes())) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "io_write", "reason": e.to_string(), "path": raw_path}),
        );
    }

    (
        StatusCode::OK,
        serde_json::json!({
            "path": raw_path,
            "length": hex.len(),
        }),
    )
}

/// Pure data computation for `/api/templates`, split out so unit tests
/// can assert on the JSON without going through hyper bodies.
fn list_templates_data() -> serde_json::Value {
    serde_json::json!([
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
    ])
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
/// testable without going through hyper bodies.
fn handle_list_fs(req: RouteRequest) -> RouteResponse {
    let (status, body) = list_fs_data(req.uri().query().unwrap_or(""));
    response::json_value(status, &body)
}

/// Pure policy: parse the query string, walk the file system, return
/// the response payload paired with the HTTP status the caller should
/// emit. No HTTP types in scope — easy to unit test.
fn list_fs_data(query: &str) -> (StatusCode, serde_json::Value) {
    let raw_path = match query::param(query, "path") {
        Some(p) if !p.is_empty() => p,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "missing_path", "hint": "GET /api/fs?path=<dir>"}),
            );
        }
    };

    if raw_path.split(['/', '\\']).any(|seg| seg == "..") {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "path_traversal", "path": raw_path}),
        );
    }

    let path = std::path::PathBuf::from(&raw_path);
    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "path_not_found", "path": raw_path}),
        );
    }
    if !path.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "not_a_directory", "path": raw_path}),
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
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": "io", "reason": e.to_string(), "path": raw_path}),
            );
        }
    };

    (
        StatusCode::OK,
        serde_json::json!({"path": raw_path, "entries": entries}),
    )
}

/// Project `kovre.yaml`'s `jobs:` block to a JSON array, attaching the
/// job name (which is the IndexMap key, not a struct field).
///
/// This route is read-only on purpose: jobs and repositories live in
/// the YAML and are the user's source of truth. The dashboard can read
/// them but cannot mutate them.
fn handle_list_jobs(cfg: Arc<Config>) -> RouteResponse {
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
    response::json_value(StatusCode::OK, &serde_json::Value::Array(body))
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
    fn extract_repo_name_for_init_handles_simple_name() {
        assert_eq!(
            extract_repo_name_for_init("/api/repositories/nas/init"),
            Some("nas".into())
        );
    }

    #[test]
    fn extract_repo_name_for_init_handles_percent_encoded() {
        assert_eq!(
            extract_repo_name_for_init("/api/repositories/local%20drive/init"),
            Some("local drive".into())
        );
    }

    #[test]
    fn extract_repo_name_for_init_rejects_wrong_suffix() {
        assert!(extract_repo_name_for_init("/api/repositories/nas/list").is_none());
    }

    #[test]
    fn list_templates_data_returns_the_four_known_templates() {
        let arr = list_templates_data();
        let names: Vec<String> = arr
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            names,
            vec!["documents", "dev-repos", "steam-saves", "custom"]
        );
    }

    #[test]
    fn get_config_data_round_trips_yaml_and_parsed() {
        let cfg = sample_cfg();
        let (status, body) = get_config_data(&cfg);
        assert_eq!(status, StatusCode::OK);
        assert!(body["yaml"].is_string());
        assert!(body["parsed"].is_object());
        assert_eq!(body["parsed"]["jobs"]["documents"]["repository"], "test");

        // The yaml field should re-parse back to the same Config.
        let yaml = body["yaml"].as_str().unwrap();
        let reparsed = Config::from_str(yaml, std::path::Path::new("test.yaml")).unwrap();
        assert!(reparsed.jobs.contains_key("documents"));
    }

    #[test]
    fn list_fs_data_lists_subdirectories() {
        let tempdir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tempdir.path().join("alpha")).unwrap();
        std::fs::create_dir(tempdir.path().join("beta")).unwrap();
        std::fs::write(tempdir.path().join("file.txt"), b"hi").unwrap();

        let (status, body) = list_fs_data(&fs_query(tempdir.path().to_str().unwrap()));
        assert_eq!(status, StatusCode::OK);
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
    fn list_fs_data_rejects_missing_path() {
        let (status, _) = list_fs_data("");
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn list_fs_data_rejects_path_traversal() {
        let (status, body) = list_fs_data(&fs_query("C:\\Users\\..\\Windows"));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "path_traversal");
    }

    #[test]
    fn list_fs_data_returns_404_for_missing_dir() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let missing = tempdir.path().join("does-not-exist");
        let (status, _) = list_fs_data(&fs_query(missing.to_str().unwrap()));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ---- /api/fs/stat ----

    #[test]
    fn fs_stat_reports_existing_file() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let file = tempdir.path().join("hi.txt");
        std::fs::write(&file, b"hello").unwrap();
        let (status, body) = list_fs_stat_data(&fs_query(file.to_str().unwrap()));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["exists"], true);
        assert_eq!(body["is_file"], true);
        assert_eq!(body["is_dir"], false);
        assert_eq!(body["size"], 5);
    }

    #[test]
    fn fs_stat_reports_missing_file() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let missing = tempdir.path().join("nope.key");
        let (status, body) = list_fs_stat_data(&fs_query(missing.to_str().unwrap()));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["exists"], false);
    }

    #[test]
    fn fs_stat_rejects_path_traversal() {
        let (status, _) = list_fs_stat_data(&fs_query("C:\\Users\\..\\Windows"));
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ---- /api/repositories/init-password ----

    #[test]
    fn init_password_writes_64_hex_chars() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let target = tempdir.path().join("test.key");
        let (status, body) = init_password_data(target.to_str().unwrap());
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["length"], 64);
        let on_disk = std::fs::read_to_string(&target).unwrap();
        assert_eq!(on_disk.len(), 64);
        assert!(on_disk.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn init_password_refuses_to_overwrite() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let target = tempdir.path().join("preexisting.key");
        std::fs::write(&target, b"do not overwrite me").unwrap();
        let (status, body) = init_password_data(target.to_str().unwrap());
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "file_exists");
        // Existing content untouched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "do not overwrite me");
    }

    #[test]
    fn init_password_creates_missing_parent_dirs() {
        let tempdir = tempfile::TempDir::new().unwrap();
        let nested = tempdir.path().join("deep").join("nest").join("key.txt");
        let (status, _) = init_password_data(nested.to_str().unwrap());
        assert_eq!(status, StatusCode::OK);
        assert!(nested.exists());
    }

    #[test]
    fn init_password_rejects_path_traversal() {
        let (status, _) = init_password_data("C:\\Users\\..\\Windows\\key.txt");
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ---- PUT /api/config ----

    /// A self-consistent YAML that we can write to disk + reload.
    fn valid_yaml(data_dir: &str) -> String {
        format!(
            "agent:\n  data_dir: {data_dir}\n  log_level: info\nrepositories:\n  test:\n    path: {data_dir}\n    password_file: {data_dir}/k.key\njobs:\n  files:\n    repository: test\n    paths:\n      - {data_dir}/source\n",
            data_dir = data_dir,
        )
    }

    fn put_config_fixture() -> (tempfile::TempDir, std::path::PathBuf, ConfigHandle) {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("kovre.yaml");
        let initial = valid_yaml(dir.path().to_str().unwrap());
        std::fs::write(&cfg_path, &initial).unwrap();
        let initial_cfg = Config::from_str(&initial, &cfg_path).unwrap();
        let swap: ConfigHandle = Arc::new(ArcSwap::from_pointee(initial_cfg));
        (dir, cfg_path, swap)
    }

    #[test]
    fn put_config_accepts_valid_yaml_writes_and_swaps() {
        let (dir, cfg_path, swap) = put_config_fixture();

        // Build a new YAML that adds a second job.
        let mut updated = valid_yaml(dir.path().to_str().unwrap());
        updated.push_str("  docs:\n    template: documents\n    repository: test\n");

        let (status, body) = put_config_data(&updated, &cfg_path, &swap);
        assert_eq!(status, StatusCode::OK, "body: {body:#}");

        // File on disk now matches what we sent.
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert_eq!(on_disk, updated);

        // In-memory swap saw the new job.
        let now = swap.load_full();
        assert!(now.jobs.contains_key("docs"));
        assert!(now.jobs.contains_key("files"));
    }

    #[test]
    fn put_config_rejects_malformed_yaml() {
        let (_dir, cfg_path, swap) = put_config_fixture();
        let snapshot_before = swap.load_full();

        let broken = "agent: !!! this is :: not yaml";
        let (status, body) = put_config_data(broken, &cfg_path, &swap);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Either yaml_parse or config_validation depending on which
        // layer rejected it; both are acceptable for malformed input.
        let kind = body["error"].as_str().unwrap_or("");
        assert!(
            kind == "yaml_parse" || kind == "config_validation",
            "unexpected error kind: {kind} (body={body:#})"
        );

        // Disk and memory both untouched.
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(on_disk.contains("files:"), "disk YAML was mutated despite parse error");
        assert!(Arc::ptr_eq(&snapshot_before, &swap.load_full()));
    }

    #[test]
    fn put_config_reports_yaml_location_when_available() {
        let (_dir, cfg_path, swap) = put_config_fixture();
        // Tab in indentation — serde_yaml flags the exact line.
        let broken =
            "agent:\n\tdata_dir: x\nrepositories: {}\njobs: {}\n";
        let (status, body) = put_config_data(broken, &cfg_path, &swap);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "yaml_parse");
        let loc = &body["location"];
        assert!(loc.is_object(), "location missing: {body:#}");
        assert!(loc["line"].as_u64().is_some());
    }

    #[test]
    fn put_config_rejects_validation_error_keeps_state() {
        let (dir, cfg_path, swap) = put_config_fixture();
        let snapshot_before = swap.load_full();

        // Parses fine but references an unknown repository.
        let invalid = format!(
            "agent:\n  data_dir: {data_dir}\n  log_level: info\nrepositories:\n  test:\n    path: {data_dir}\n    password_file: {data_dir}/k.key\njobs:\n  oops:\n    template: documents\n    repository: ghost\n",
            data_dir = dir.path().to_str().unwrap(),
        );
        let (status, body) = put_config_data(&invalid, &cfg_path, &swap);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "config_validation");

        // Disk + memory unchanged.
        let on_disk = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(on_disk.contains("files:"));
        assert!(!on_disk.contains("oops:"));
        assert!(Arc::ptr_eq(&snapshot_before, &swap.load_full()));
    }

    #[test]
    fn put_config_round_trip_via_get_config() {
        let (dir, cfg_path, swap) = put_config_fixture();

        // Round-trip: read the current in-memory YAML via the GET helper,
        // PUT it back, then re-GET. Should arrive at the same content.
        let snapshot = swap.load_full();
        let (_, first) = get_config_data(&snapshot);
        let yaml_v1 = first["yaml"].as_str().unwrap().to_string();

        let (status, _) = put_config_data(&yaml_v1, &cfg_path, &swap);
        assert_eq!(status, StatusCode::OK);

        let snapshot2 = swap.load_full();
        let (_, second) = get_config_data(&snapshot2);
        let yaml_v2 = second["yaml"].as_str().unwrap();
        assert_eq!(yaml_v1, yaml_v2, "round-trip diverged");
    }

    // ---- helpers ----

    fn sample_cfg() -> Config {
        use indexmap::IndexMap;
        use kovre_core::config::{Agent, BackendKind, Job, Repository};
        use std::path::PathBuf;
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
}
