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

    // GET /api/templates — the 7 builtin templates, in display order.
    // Order is enforced because it drives the wizard's gallery layout:
    // personal-data templates first, dev/games next, safety net + escape
    // hatch last.
    let templates = get_json(&a, "/api/templates");
    let names: Vec<String> = templates
        .as_array()
        .expect("templates is array")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "user-files",
            "thunderbird-mail",
            "browser-profiles",
            "dev-repos",
            "steam-saves",
            "user-appdata",
            "custom"
        ]
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

    // ---- Phase 4: full mirror backend pipeline via the dashboard API ----
    // Adds a mirror repo + job through PUT /api/config (live reload),
    // initializes, runs three backups against a mutating source and
    // asserts:
    //   - canonical files land at <repo>/<job>/<basename>/...
    //   - overwritten files are preserved in .versions/<rel>/<stem>-<ts>.<ext>
    //   - keep_versions retention prunes once we exceed the budget.

    let workspace_root = fx.workspace.path();
    let mirror_src = workspace_root.join("mirror_src");
    let mirror_repo = workspace_root.join("mirror_repo");
    std::fs::create_dir_all(&mirror_src).unwrap();
    std::fs::write(mirror_src.join("photo.jpg"), b"v1-photo").unwrap();
    std::fs::write(mirror_src.join("doc.txt"), b"v1-doc-content").unwrap();

    // Build a fresh YAML carrying everything kovre.yaml already had
    // plus the mirror repo + photos-mirror job. We rebuild from
    // scratch rather than patching the in-memory yaml string because
    // serde_yaml normalizes whitespace on the round-trip and partial
    // string ops are brittle.
    let workspace_data_dir = workspace_root.join("data");
    let rustic_repo = workspace_root.join("repo");
    let rustic_pwd = workspace_root.join("repo.key");
    let rustic_src = workspace_root.join("source");

    let full_yaml = format!(
        concat!(
            "agent:\n",
            "  data_dir: {data}\n",
            "  log_level: warn\n",
            "repositories:\n",
            "  test:\n",
            "    path: {rustic_repo}\n",
            "    password_file: {rustic_pwd}\n",
            "  photos:\n",
            "    path: {mirror_repo}\n",
            "    backend: mirror\n",
            "jobs:\n",
            "  files:\n",
            "    repository: test\n",
            "    paths:\n",
            "      - {rustic_src}\n",
            "  added_by_test:\n",
            "    template: documents\n",
            "    repository: test\n",
            "  photos-mirror:\n",
            "    repository: photos\n",
            "    paths:\n",
            "      - {mirror_src}\n",
            "    retention:\n",
            "      keep_versions: 2\n",
        ),
        data = yaml_path(&workspace_data_dir),
        rustic_repo = yaml_path(&rustic_repo),
        rustic_pwd = yaml_path(&rustic_pwd),
        rustic_src = yaml_path(&rustic_src),
        mirror_repo = yaml_path(&mirror_repo),
        mirror_src = yaml_path(&mirror_src),
    );

    let (cfg_status, cfg_body) = put_yaml(&a, "/api/config", &full_yaml);
    assert_eq!(
        cfg_status, 200,
        "PUT /api/config with mirror entry failed: {cfg_body:#}"
    );
    assert!(
        cfg_body["parsed"]["jobs"]["photos-mirror"].is_object(),
        "expected photos-mirror to be in parsed response: {cfg_body:#}"
    );

    // mirror verify is a no-op but the route must succeed.
    let (mverify_status, mverify_body) =
        post_status(&a, "/api/repositories/photos/verify");
    assert_eq!(mverify_status, 200);
    assert_eq!(mverify_body["ok"], true);
    assert!(
        mverify_body["messages"]
            .as_array()
            .map(|arr| arr.iter().any(|m| m.as_str().unwrap_or("").contains("mirror")))
            .unwrap_or(false),
        "mirror verify should mention the backend: {mverify_body:#}"
    );

    // Init creates the dest root (mkdir -p). Idempotent on mirror.
    let (minit_status, _) = post_status(&a, "/api/repositories/photos/init");
    assert_eq!(minit_status, 200, "mirror init should always succeed");

    // --- First run: canonical files land in the mirror.
    let mirror_canonical = mirror_repo.join("photos-mirror").join("mirror_src");
    let mirror_versions = mirror_repo.join("photos-mirror").join(".versions");

    let (m_run1_status, m_run1_body) = post_status(&a, "/api/jobs/photos-mirror/run");
    assert_eq!(m_run1_status, 202, "first mirror run not accepted");
    let m_run1_id = m_run1_body["id"].as_str().unwrap().to_string();
    poll_run_until_terminal(&a, &m_run1_id);
    assert_eq!(
        std::fs::read(mirror_canonical.join("photo.jpg")).unwrap(),
        b"v1-photo"
    );
    assert_eq!(
        std::fs::read(mirror_canonical.join("doc.txt")).unwrap(),
        b"v1-doc-content"
    );
    // No archived versions yet — nothing was overwritten.
    let v1_count = count_files(&mirror_versions);
    assert_eq!(v1_count, 0, ".versions/ should be empty after first run");

    // --- Second run: modify photo.jpg, doc.txt unchanged.
    // Sleep ≥1s so the mtime/size delta is observable on filesystems
    // that round timestamps to the nearest second.
    thread::sleep(Duration::from_millis(1100));
    std::fs::write(mirror_src.join("photo.jpg"), b"v2-photo-longer").unwrap();

    let (m_run2_status, m_run2_body) = post_status(&a, "/api/jobs/photos-mirror/run");
    assert_eq!(m_run2_status, 202);
    poll_run_until_terminal(&a, m_run2_body["id"].as_str().unwrap());

    assert_eq!(
        std::fs::read(mirror_canonical.join("photo.jpg")).unwrap(),
        b"v2-photo-longer",
        "canonical should hold the new version"
    );
    let v2_count = count_files(&mirror_versions);
    assert_eq!(v2_count, 1, "1 archived version expected after one overwrite");

    // --- Third run: modify photo.jpg again. Now we'd have 2 versions
    //   in .versions/ → still within keep_versions=2 budget, no prune.
    thread::sleep(Duration::from_millis(1100));
    std::fs::write(mirror_src.join("photo.jpg"), b"v3-photo-much-longer").unwrap();

    let (m_run3_status, m_run3_body) = post_status(&a, "/api/jobs/photos-mirror/run");
    assert_eq!(m_run3_status, 202);
    poll_run_until_terminal(&a, m_run3_body["id"].as_str().unwrap());

    let v3_count = count_files(&mirror_versions);
    assert_eq!(
        v3_count, 2,
        "2 archived versions expected (within keep_versions=2 budget)"
    );

    // --- Fourth run: modify photo.jpg once more → would have 3
    //   archives, retention prunes the oldest → back to 2.
    thread::sleep(Duration::from_millis(1100));
    std::fs::write(mirror_src.join("photo.jpg"), b"v4-photo-even-more").unwrap();

    let (m_run4_status, m_run4_body) = post_status(&a, "/api/jobs/photos-mirror/run");
    assert_eq!(m_run4_status, 202);
    poll_run_until_terminal(&a, m_run4_body["id"].as_str().unwrap());

    let v4_count = count_files(&mirror_versions);
    assert_eq!(
        v4_count, 2,
        "retention should have pruned the oldest archive (got {v4_count} versions)"
    );

    // ---- Phase 6: restore round-trip via the API ----
    // The mirror job has been backed up (canonical = v4-photo, doc = v1-doc).
    // Trigger a restore into a fresh dest and assert the content matches.

    let restore_dest = workspace_root.join("restore_dest");
    let dest_str = restore_dest.to_string_lossy();
    let (restore_status, restore_body) = post_json(
        &a,
        "/api/jobs/photos-mirror/restore",
        &serde_json::json!({ "dest_dir": dest_str }),
    );
    assert_eq!(
        restore_status, 202,
        "restore should be accepted: {restore_body:#}"
    );
    let restore_id = restore_body["id"].as_str().expect("restore id");
    poll_restore_until_terminal(&a, restore_id);

    // The mirror engine's restore_latest copies `<repo>/<job>/<basename>/…`
    // into `<dest>/<basename>/…`. So the source's basename is `mirror_src`.
    let restored_root = restore_dest.join("mirror_src");
    assert!(
        restored_root.is_dir(),
        "restored root missing: {}",
        restored_root.display()
    );
    assert_eq!(
        fs::read(restored_root.join("photo.jpg")).unwrap(),
        b"v4-photo-even-more",
        "restore should reflect the latest canonical content"
    );
    assert_eq!(
        fs::read(restored_root.join("doc.txt")).unwrap(),
        b"v1-doc-content"
    );

    // Restore unknown job → 404.
    let (r404_status, r404_body) = post_json(
        &a,
        "/api/jobs/ghost/restore",
        &serde_json::json!({ "dest_dir": dest_str }),
    );
    assert_eq!(r404_status, 404);
    assert_eq!(r404_body["error"], "unknown_job");

    // Restore with invalid dest_dir → 400.
    let (r400_status, r400_body) = post_json(
        &a,
        "/api/jobs/photos-mirror/restore",
        &serde_json::json!({ "dest_dir": r"C:\Users\..\Windows" }),
    );
    assert_eq!(r400_status, 400);
    assert_eq!(r400_body["error"], "invalid_dest");
}

/// POST with a JSON body, returning `(status_code, response_json)`.
/// Mirrors `post_status` but takes a serializable body.
fn post_json(a: &ureq::Agent, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let resp = a
        .post(&url(path))
        .set("content-type", "application/json")
        .send_string(&body.to_string());
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

/// Poll `/api/job_runs/<run_id>` until status is `success` or `failed`.
/// Panics with the full run body on failure or after a 60s timeout.
fn poll_run_until_terminal(a: &ureq::Agent, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let single = get_json(a, &format!("/api/job_runs/{run_id}"));
        let s = single["status"].as_str().unwrap_or("").to_string();
        if s == "success" {
            return;
        }
        if s == "failed" {
            panic!("run {run_id} failed: {single:#}");
        }
        if Instant::now() >= deadline {
            panic!("run {run_id} never reached terminal state (last status={s})");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Poll `/api/restore_runs/<run_id>` until status is `success` or
/// `failed`. Same shape as `poll_run_until_terminal` but for the
/// restore model.
fn poll_restore_until_terminal(a: &ureq::Agent, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let single = get_json(a, &format!("/api/restore_runs/{run_id}"));
        let s = single["status"].as_str().unwrap_or("").to_string();
        if s == "success" {
            return;
        }
        if s == "failed" {
            panic!("restore {run_id} failed: {single:#}");
        }
        if Instant::now() >= deadline {
            panic!("restore {run_id} never reached terminal state (last status={s})");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Count regular files under `root` (recursive). Returns 0 if `root`
/// doesn't exist, which is the expected state before the first
/// archive happens.
fn count_files(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
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
