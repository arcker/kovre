//! End-to-end integration test for `kovre serve`.
//!
//! Spawns the compiled `kovre` binary against a temporary rustic
//! repository, drives the dashboard's HTTP surface from the outside
//! (HTTP requests, not in-process calls), and checks the canonical
//! flows that the Phase 2 DoD asks for:
//!
//!   - `/health`, `/ready`, `/info` answer 200,
//!   - `GET /api/jobs` reflects `kovre.yaml`,
//!   - `POST /api/jobs/:name/run` triggers a backup that ends in
//!     `success`,
//!   - `GET /api/job_runs/<id>` reflects the terminal state,
//!   - a re-trigger while the first run is still going gets 409,
//!   - `POST /api/sync` re-projects snapshots,
//!   - the embedded SvelteKit `/` shell is served and
//!     unknown app routes (`/jobs/<name>`) fall back to the same
//!     shell while unknown API paths get a JSON 404,
//!   - `--debug` exposes Lithair's `/_admin` panel.
//!
//! The test runs with a single `kovre serve` instance on a fixed
//! local port; a separate test binary doing the same in parallel
//! would conflict, but `cargo test --test dashboard` runs all of
//! its `#[test]` functions serially by default since this file
//! contains only one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TEST_PORT: u16 = 19283;
const HOST: &str = "127.0.0.1";

fn kovre_bin() -> &'static str {
    env!("CARGO_BIN_EXE_kovre")
}

fn yaml_path(p: &Path) -> String {
    let mut s = String::from("\"");
    for c in p.to_string_lossy().chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            other => s.push(other),
        }
    }
    s.push('"');
    s
}

struct Fixture {
    workspace: TempDir,
    config: PathBuf,
}

impl Fixture {
    fn build() -> Self {
        let workspace = TempDir::new().expect("temp dir");
        let root = workspace.path();

        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("hello.txt"), b"hello world\n").unwrap();

        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let password_file = root.join("repo.key");
        fs::write(&password_file, b"test-pass\n").unwrap();

        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let config = root.join("kovre.yaml");
        let yaml = format!(
            "agent:\n  data_dir: {data}\n  log_level: warn\nrepositories:\n  test:\n    path: {repo}\n    password_file: {pwd}\njobs:\n  files:\n    repository: test\n    paths:\n      - {src}\n",
            data = yaml_path(&data_dir),
            repo = yaml_path(&repo),
            pwd = yaml_path(&password_file),
            src = yaml_path(&source),
        );
        fs::write(&config, yaml).unwrap();

        // Initialize the rustic repo via the CLI before bringing the
        // dashboard up, so the boot-time snapshot sync has nothing to
        // panic on.
        let init = Command::new(kovre_bin())
            .arg("--config")
            .arg(&config)
            .args(["init-repo", "test"])
            .output()
            .expect("spawn init-repo");
        assert!(
            init.status.success(),
            "init-repo failed:\n{}",
            String::from_utf8_lossy(&init.stderr)
        );

        Self { workspace, config }
    }
}

/// Started `kovre serve` instance. `Drop` kills the child so a panic
/// in the test body never leaves a dangling process bound to the
/// fixed test port.
struct ServeProcess {
    child: Child,
}

impl ServeProcess {
    fn spawn(config: &Path) -> Self {
        let child = Command::new(kovre_bin())
            .arg("--config")
            .arg(config)
            .args(["serve", "--port", &TEST_PORT.to_string(), "--debug"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn kovre serve");

        // Wait up to 30s for /health to start answering. On Windows
        // the first run after a cold cargo build can take a moment.
        // We reuse a single Agent here too so the readiness probe does
        // not flood the loopback with TIME_WAIT sockets.
        let probe = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(500))
            .build();
        let deadline = Instant::now() + Duration::from_secs(30);
        let url = format!("http://{HOST}:{TEST_PORT}/health");
        loop {
            if let Ok(resp) = probe.get(&url).call() {
                if resp.status() == 200 {
                    break;
                }
            }
            if Instant::now() >= deadline {
                panic!("kovre serve did not respond on {url} within 30s");
            }
            thread::sleep(Duration::from_millis(200));
        }

        Self { child }
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Ok(out) = self.child.wait_with_output_safe() {
            if std::thread::panicking() {
                if !out.0.is_empty() {
                    eprintln!("--- kovre serve stdout ---\n{}", out.0);
                }
                if !out.1.is_empty() {
                    eprintln!("--- kovre serve stderr ---\n{}", out.1);
                }
            }
        }
    }
}

trait ChildOutputExt {
    /// Best-effort capture of remaining stdout/stderr without blocking.
    /// Used in `Drop` so a panicking test surfaces the server's logs.
    fn wait_with_output_safe(&mut self) -> std::io::Result<(String, String)>;
}

impl ChildOutputExt for Child {
    fn wait_with_output_safe(&mut self) -> std::io::Result<(String, String)> {
        use std::io::Read;
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut s) = self.stdout.take() {
            let _ = s.read_to_string(&mut stdout);
        }
        if let Some(mut s) = self.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let _ = self.wait();
        Ok((stdout, stderr))
    }
}

fn url(path: &str) -> String {
    format!("http://{HOST}:{TEST_PORT}{path}")
}

fn agent() -> ureq::Agent {
    // A shared agent reuses TCP connections across requests, which is
    // the only sensible thing to do on Windows: a poll loop that opens
    // a new socket per request burns through the ephemeral port range
    // and hits TIME_WAIT exhaustion on the localhost loopback after
    // ~30s of activity, manifesting as connect timeouts that look like
    // server crashes but are not.
    ureq::AgentBuilder::new()
        // /api/sync re-opens the rustic repository to enumerate snapshots
        // and that occasionally takes several seconds right after a fresh
        // backup, especially when the OS file cache is cold. 60s is well
        // beyond what the actual operation needs but cheap insurance.
        .timeout(Duration::from_secs(60))
        .build()
}

fn get_json(agent: &ureq::Agent, path: &str) -> serde_json::Value {
    agent
        .get(&url(path))
        .call()
        .unwrap_or_else(|e| panic!("GET {path}: {e}"))
        .into_json()
        .unwrap_or_else(|e| panic!("GET {path}: bad JSON: {e}"))
}

fn post_status(agent: &ureq::Agent, path: &str) -> (u16, serde_json::Value) {
    let resp = agent.post(&url(path)).send_string("");
    match resp {
        Ok(r) => {
            let s = r.status();
            (s, r.into_json().unwrap_or(serde_json::json!({})))
        }
        Err(ureq::Error::Status(code, r)) => {
            (code, r.into_json().unwrap_or(serde_json::json!({})))
        }
        Err(e) => panic!("POST {path}: {e}"),
    }
}

fn put_yaml(agent: &ureq::Agent, path: &str, body: &str) -> (u16, serde_json::Value) {
    let resp = agent
        .put(&url(path))
        .set("content-type", "application/yaml")
        .send_string(body);
    match resp {
        Ok(r) => {
            let s = r.status();
            (s, r.into_json().unwrap_or(serde_json::json!({})))
        }
        Err(ureq::Error::Status(code, r)) => {
            (code, r.into_json().unwrap_or(serde_json::json!({})))
        }
        Err(e) => panic!("PUT {path}: {e}"),
    }
}

#[test]
fn dashboard_end_to_end() {
    let fx = Fixture::build();
    let _serve = ServeProcess::spawn(&fx.config);
    let a = agent();

    // ---- Lithair built-ins ----
    let health = get_json(&a, "/health");
    assert_eq!(health["status"], "healthy");
    let ready = get_json(&a, "/ready");
    assert_eq!(ready["status"], "ready");
    let info = get_json(&a, "/info");
    assert_eq!(info["server"], "Lithair Server");

    // ---- /api/jobs reflects kovre.yaml ----
    let jobs = get_json(&a, "/api/jobs");
    let arr = jobs.as_array().expect("jobs is array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "files");
    assert_eq!(arr[0]["repository"], "test");

    // ---- empty job_runs at boot ----
    let runs = get_json(&a, "/api/job_runs");
    assert_eq!(runs["total"], 0);

    // ---- trigger a backup ----
    let (status, body) = post_status(&a, "/api/jobs/files/run");
    assert_eq!(status, 202);
    let run_id = body["id"].as_str().expect("id").to_string();

    // ---- a second trigger while the first is still running -> 409 ----
    let (status2, body2) = post_status(&a, "/api/jobs/files/run");
    assert_eq!(status2, 409);
    assert_eq!(body2["error"], "already_running");
    assert_eq!(body2["run_id"], run_id);

    // ---- unknown job -> 404 ----
    let (status3, body3) = post_status(&a, "/api/jobs/ghost/run");
    assert_eq!(status3, 404);
    assert_eq!(body3["error"], "unknown_job");

    // ---- poll until terminal state ----
    let deadline = Instant::now() + Duration::from_secs(60);
    let final_status = loop {
        let single = get_json(&a, &format!("/api/job_runs/{run_id}"));
        let s = single["status"].as_str().unwrap_or("").to_string();
        if s == "success" || s == "failed" {
            break (s, single);
        }
        if Instant::now() >= deadline {
            panic!("run {run_id} never reached a terminal state (last status={s})");
        }
        thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(final_status.0, "success", "run record: {:#}", final_status.1);
    assert!(
        final_status.1["snapshot_id"].as_str().is_some(),
        "expected snapshot_id to be set on success: {:#}",
        final_status.1
    );

    // ---- after success, sync should pull the new snapshot ----
    let (sync_status, sync_body) = post_status(&a, "/api/sync");
    assert_eq!(sync_status, 200);
    assert!(sync_body["synced"].as_u64().unwrap_or(0) >= 1);
    let snaps = get_json(&a, "/api/snapshots");
    assert!(
        snaps["total"].as_u64().unwrap_or(0) >= 1,
        "expected ≥1 snapshot after sync: {snaps:#}"
    );

    // ---- embedded frontend: SPA shell on / and on unknown app paths ----
    let root_resp = a
        .get(&url("/"))
        .call()
        .expect("GET /")
        .into_string()
        .expect("body");
    assert!(
        root_resp.contains("<title>kovre dashboard</title>"),
        "GET / did not return the SvelteKit shell — was the frontend built? \
         Run `npm --prefix web run build` before `cargo test`."
    );

    let unknown_route_body = a
        .get(&url("/jobs/files"))
        .call()
        .expect("GET /jobs/files (SPA fallback)")
        .into_string()
        .expect("body");
    assert!(
        unknown_route_body.contains("<title>kovre dashboard</title>"),
        "/jobs/files should fall back to the SPA shell"
    );

    // ---- unknown /api/* paths return JSON 404, NOT the SPA shell ----
    let api_404 = a.get(&url("/api/does_not_exist")).call();
    let (api_404_status, api_404_body) = match api_404 {
        Ok(_) => panic!("expected 404 on unknown api path"),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => panic!("unexpected error: {e}"),
    };
    assert_eq!(api_404_status, 404);
    assert!(api_404_body.contains("\"not_found\""));

    // ---- --debug exposes the admin panel at /_admin ----
    let admin_status = a
        .get(&url("/_admin"))
        .call()
        .map(|r| r.status())
        .unwrap_or_else(|e| match e {
            ureq::Error::Status(code, _) => code,
            other => panic!("/_admin probe failed: {other}"),
        });
    assert_eq!(admin_status, 200, "--debug should enable /_admin");

    // ---- Phase 3 read-only API ----

    // GET /api/templates — the four builtin templates, in stable order.
    let templates = get_json(&a, "/api/templates");
    let names: Vec<String> = templates
        .as_array()
        .expect("templates is array")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        names,
        vec!["documents", "dev-repos", "steam-saves", "custom"]
    );

    // GET /api/fs — the workspace root contains the source/repo/data
    // directories the fixture created.
    let workspace_path = fx.workspace.path().to_string_lossy().into_owned();
    let fs = get_json(
        &a,
        &format!("/api/fs?path={}", urlencode(&workspace_path)),
    );
    let entries: Vec<String> = fs["entries"]
        .as_array()
        .expect("entries is array")
        .iter()
        .map(|e| e["name"].as_str().unwrap_or("").to_string())
        .collect();
    for expected in ["source", "repo", "data"] {
        assert!(
            entries.contains(&expected.to_string()),
            "/api/fs should list `{expected}` under the workspace (got {entries:?})"
        );
    }

    // GET /api/config — yaml + parsed mirror what the server holds in
    // memory.
    let cfg_before = get_json(&a, "/api/config");
    assert!(cfg_before["yaml"].is_string());
    let yaml_before = cfg_before["yaml"].as_str().unwrap().to_string();
    assert!(yaml_before.contains("files:"));
    assert!(!yaml_before.contains("added_by_test:"));

    // ---- Phase 3 PUT /api/config: invalid YAML stays out ----
    let (status_bad, body_bad) = put_yaml(&a, "/api/config", "agent: !!! not yaml");
    assert_eq!(status_bad, 400);
    let err_kind = body_bad["error"].as_str().unwrap_or("");
    assert!(
        err_kind == "yaml_parse" || err_kind == "config_validation",
        "unexpected error kind on bad YAML: {err_kind} (body={body_bad:#})"
    );

    // The bad PUT must not have mutated state.
    let cfg_check = get_json(&a, "/api/config");
    assert_eq!(cfg_check["yaml"].as_str().unwrap(), yaml_before);

    // ---- Phase 3 PUT /api/config: valid YAML reloads live ----
    let mut yaml_after = yaml_before.trim_end().to_string();
    yaml_after.push_str("\n  added_by_test:\n    template: documents\n    repository: test\n");

    let (status_ok, body_ok) = put_yaml(&a, "/api/config", &yaml_after);
    assert_eq!(status_ok, 200, "PUT response: {body_ok:#}");
    assert!(
        body_ok["parsed"]["jobs"]["added_by_test"].is_object(),
        "response should reflect the newly-added job: {body_ok:#}"
    );

    // GET /api/jobs sees it without a restart (ArcSwap live reload).
    let jobs_after = get_json(&a, "/api/jobs");
    let job_names: Vec<String> = jobs_after
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        job_names.contains(&"added_by_test".into()) && job_names.contains(&"files".into()),
        "live reload missed the new job: {job_names:?}"
    );

    // The file on disk got rewritten atomically.
    let on_disk = std::fs::read_to_string(&fx.config).expect("read kovre.yaml");
    assert!(on_disk.contains("added_by_test:"));
    assert!(on_disk.contains("files:"));

    // ---- POST /api/repositories/:name/verify — rustic happy path ----
    let (verify_status, verify_body) = post_status(&a, "/api/repositories/test/verify");
    assert_eq!(
        verify_status, 200,
        "verify should succeed on a freshly-initialized repo: {verify_body:#}"
    );
    assert_eq!(verify_body["ok"], true);
    assert_eq!(verify_body["name"], "test");
    assert!(verify_body["messages"].is_array(), "expected messages array");

    // Unknown repository → 404.
    let (verify_404_status, verify_404_body) =
        post_status(&a, "/api/repositories/ghost/verify");
    assert_eq!(verify_404_status, 404);
    assert_eq!(verify_404_body["error"], "unknown_repository");
}

/// Minimal URL-encoder for path values (Windows backslash, colon,
/// spaces). Avoids pulling a percent-encoding crate just for one test.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
