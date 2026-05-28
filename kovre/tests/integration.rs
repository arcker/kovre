//! End-to-end integration tests for kovre.
//!
//! These tests drive the compiled `kovre` binary against a temporary
//! filesystem repository and validate that:
//!   - a job's `excludes:` are honored (no `.tmp` ends up in the snapshot),
//!   - retention deletes the right snapshots and keeps the rest,
//!   - `run --all` does not abort sibling jobs when one fails.
//!
//! Restore-and-diff is intentionally exercised through `rustic_core`'s own
//! repository APIs rather than by shelling out to the standalone `rustic`
//! CLI: the latter would force every developer (and CI) to install rustic
//! globally just to run `cargo test`. Format compatibility with the
//! standalone CLI is documented as a manual validation step in `README.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rustic_backend::BackendOptions;
use rustic_core::repofile::SnapshotFile;
use rustic_core::{Credentials, Repository, RepositoryOptions};
use tempfile::TempDir;

/// Path to the `kovre` binary that Cargo built for this test run.
fn kovre_bin() -> &'static str {
    env!("CARGO_BIN_EXE_kovre")
}

/// Materialize a minimal `kovre.yaml` plus a password file and a populated
/// source tree, all under one TempDir. The `_workspace` and `_source`
/// fields are kept to extend the TempDir's lifetime and to leave room for
/// future tests that need to mutate the source tree mid-run.
#[allow(dead_code)]
struct Fixture {
    _workspace: TempDir,
    config: PathBuf,
    repo: PathBuf,
    password_file: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(yaml_body: &str, source_files: &[(&str, &str)]) -> Self {
        let workspace = TempDir::new().expect("create temp dir");
        let root = workspace.path();

        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        for (rel, content) in source_files {
            let p = source.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, content).unwrap();
        }

        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let password_file = root.join("repo.key");
        fs::write(&password_file, "test-passphrase\n").unwrap();

        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let config = root.join("kovre.yaml");
        let yaml = yaml_body
            .replace("{REPO}", &yaml_path(&repo))
            .replace("{PASSWORD_FILE}", &yaml_path(&password_file))
            .replace("{SOURCE}", &yaml_path(&source))
            .replace("{DATA_DIR}", &yaml_path(&data_dir));
        fs::write(&config, yaml).unwrap();

        Self {
            _workspace: workspace,
            config,
            repo,
            password_file,
            source,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(kovre_bin());
        cmd.arg("--config").arg(&self.config);
        for a in args {
            cmd.arg(a);
        }
        let out = cmd.output().expect("spawn kovre");
        if !out.status.success() {
            panic!(
                "kovre {args:?} failed (status {:?})\nstdout:\n{}\nstderr:\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
        out
    }

}

/// YAML-quote a path: escape backslashes and wrap in double quotes so the
/// resulting `path: "..."` line is unambiguous on Windows.
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

/// Open the repository directly via `rustic_core` and return all snapshots
/// it contains. Used by tests that need to validate snapshot summaries.
fn read_all_snapshots(repo_path: &Path, password_file: &Path) -> Vec<SnapshotFile> {
    let backends = BackendOptions::default()
        .repository(repo_path.to_string_lossy().to_string())
        .to_backends()
        .expect("to_backends");
    let pass = fs::read_to_string(password_file).unwrap();
    let creds = Credentials::Password(pass.lines().next().unwrap().to_string());
    Repository::new(&RepositoryOptions::default(), &backends)
        .expect("Repository::new")
        .open(&creds)
        .expect("Repository::open")
        .get_all_snapshots()
        .expect("get_all_snapshots")
}

#[test]
fn backup_creates_snapshot_and_excludes_tmp_files() {
    let yaml = r#"
agent:
  data_dir: {DATA_DIR}
  log_level: warn
repositories:
  test:
    path: {REPO}
    password_file: {PASSWORD_FILE}
jobs:
  files:
    repository: test
    paths:
      - {SOURCE}
    excludes:
      - "**/*.tmp"
"#;
    let fx = Fixture::new(
        yaml,
        &[
            ("hello.txt", "Hello, world!\n"),
            ("notes.md", "# Notes\nLine two.\n"),
            ("ignored.tmp", "should be excluded"),
            ("nested/deep.txt", "deep file\n"),
            ("nested/another.tmp", "also excluded"),
        ],
    );

    fx.run(&["init-repo", "test"]);
    fx.run(&["run", "files"]);

    let listing = fx.run(&["list-snapshots", "files"]);
    let stdout = String::from_utf8_lossy(&listing.stdout);
    // Header row + at least one data row.
    assert!(stdout.contains("ID"), "list-snapshots header missing: {stdout}");
    let snapshot_lines = stdout
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("ID") && !l.starts_with("---"))
        .count();
    assert_eq!(snapshot_lines, 1, "expected exactly one snapshot, stdout was:\n{stdout}");

    // Open the repository directly to inspect the snapshot summary.
    let snaps = read_all_snapshots(&fx.repo, &fx.password_file);
    assert_eq!(snaps.len(), 1, "exactly one snapshot expected");

    let summary = snaps[0]
        .summary
        .as_ref()
        .expect("snapshot summary should be present after a fresh backup");
    // 3 .txt/.md files committed; the 2 .tmp files must be excluded.
    assert_eq!(
        summary.total_files_processed, 3,
        "exclude `**/*.tmp` ignored — got {} files in snapshot",
        summary.total_files_processed,
    );

    // Tag must mark the snapshot as belonging to the `files` job.
    assert!(
        snaps[0].tags.contains(&"kovre-job:files".to_string()),
        "expected job tag, got tags = {:?}",
        snaps[0].tags,
    );

    drop(fx); // keep the workspace alive until here
}

#[test]
fn retention_keep_last_forgets_older_snapshots() {
    use kovre_core::backup::{engine_for, BackupSource};
    use kovre_core::config::{Repository as RepoConfig, Retention};

    let workspace = TempDir::new().unwrap();
    let root = workspace.path();
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.txt"), "a").unwrap();

    let repo_path = root.join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    let password_file = root.join("repo.key");
    fs::write(&password_file, "test-pass\n").unwrap();

    let repo_cfg = RepoConfig {
        path: repo_path.clone(),
        backend: kovre_core::config::BackendKind::Rustic,
        password_file: Some(password_file.clone()),
    smb_user: None,
    smb_password_file: None,
    };

    engine_for(&repo_cfg).init().unwrap();

    // Create 5 snapshots back-to-back. Each iteration mutates the source so
    // the new snapshot is meaningfully distinct (rustic still creates a snapshot
    // even with identical content, but writing a new byte exercises the index too).
    for i in 0..5 {
        fs::write(source.join("a.txt"), format!("a{i}")).unwrap();
        engine_for(&repo_cfg)
            .backup(
                "job1",
                BackupSource {
                    paths: vec![source.clone()],
                    excludes: vec![],
                },
            )
            .unwrap();
    }

    let before = engine_for(&repo_cfg).list_snapshots("job1").unwrap();
    assert_eq!(before.len(), 5, "expected 5 snapshots before retention");

    let retention = Retention {
        keep_last: Some(2),
        ..Default::default()
    };
    let outcome = engine_for(&repo_cfg)
        .apply_retention("job1", &retention)
        .unwrap();
    assert_eq!(outcome.kept, 2, "outcome.kept");
    assert_eq!(outcome.forgotten, 3, "outcome.forgotten");

    let after = engine_for(&repo_cfg).list_snapshots("job1").unwrap();
    assert_eq!(after.len(), 2, "snapshots remaining after retention");
}

#[test]
fn run_all_continues_when_one_job_fails() {
    let workspace = TempDir::new().unwrap();
    let root = workspace.path();

    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.txt"), "hello").unwrap();

    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let password_file = root.join("repo.key");
    fs::write(&password_file, "test-pass\n").unwrap();
    let data_dir = root.join("data");
    fs::create_dir_all(&data_dir).unwrap();

    let bad_path = root.join("does-not-exist");

    let config = root.join("kovre.yaml");
    let yaml = format!(
        "agent:\n  data_dir: {data}\n  log_level: warn\nrepositories:\n  test:\n    path: {repo_p}\n    password_file: {pwd}\njobs:\n  good:\n    repository: test\n    paths:\n      - {src}\n  bad:\n    repository: test\n    paths:\n      - {bad}\n",
        data = yaml_path(&data_dir),
        repo_p = yaml_path(&repo),
        pwd = yaml_path(&password_file),
        src = yaml_path(&source),
        bad = yaml_path(&bad_path),
    );
    fs::write(&config, yaml).unwrap();

    let bin = kovre_bin();
    let init = Command::new(bin)
        .args(["--config"])
        .arg(&config)
        .args(["init-repo", "test"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init-repo failed:\n{}",
        String::from_utf8_lossy(&init.stderr)
    );

    // `run --all` should exit non-zero because `bad` fails, but `good` must
    // still produce a snapshot before that exit.
    let run_all = Command::new(bin)
        .args(["--config"])
        .arg(&config)
        .args(["run", "--all"])
        .output()
        .unwrap();
    assert!(
        !run_all.status.success(),
        "run --all should have failed because of the `bad` job"
    );

    let listing = Command::new(bin)
        .args(["--config"])
        .arg(&config)
        .args(["list-snapshots", "good"])
        .output()
        .unwrap();
    assert!(
        listing.status.success(),
        "list-snapshots good failed:\n{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let stdout = String::from_utf8_lossy(&listing.stdout);
    let good_snapshots = stdout
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("ID") && !l.starts_with("---"))
        .count();
    assert_eq!(
        good_snapshots, 1,
        "expected `good` to produce its snapshot despite `bad` failing; got:\n{stdout}",
    );
}

// ---------------------------------------------------------------------
// Restore round-trip tests
//
// The reliability claim of any backup tool is: "if I back this up, I
// can get it back, byte-for-byte." These two tests assert that claim
// for each backend by going backup → restore → walk both trees and
// diff their contents. The fixture mixes text, binary, accents,
// spaces, nesting and an empty file to maximize the chance of catching
// path/encoding regressions.
// ---------------------------------------------------------------------

/// Fixture content shared by both restore tests. The keys are forward-
/// slash relative paths (will be split on `/` when materialized).
fn restore_fixture_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("hello.txt", b"Hello, world!\n".to_vec()),
        ("empty.bin", Vec::new()),
        ("nested/deep/notes.md", b"# Title\n\nBody.\n".to_vec()),
        (
            "with spaces.txt",
            "Ligne avec espaces et accents éàü.\n".as_bytes().to_vec(),
        ),
        // 4 KiB of pseudo-random bytes so we exercise the chunker /
        // copy path beyond ASCII.
        (
            "binary.dat",
            (0u16..4096).map(|i| (i % 251) as u8).collect(),
        ),
    ]
}

fn write_source_tree(root: &Path, files: &[(&str, Vec<u8>)]) {
    for (rel, content) in files {
        let path = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }
}

/// Walk `root` and return every regular file's relative path + content,
/// sorted by relative path. The sort makes assertions order-independent.
fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e.path().strip_prefix(root).unwrap().to_path_buf();
            let bytes = fs::read(e.path()).unwrap();
            (rel, bytes)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Walk `dest` and return the parent directory of the first file
/// whose basename matches `sentinel`. Used to locate the restored
/// source tree without hard-coding the backend's path conventions
/// (rustic stores the absolute source path; mirror uses the source
/// basename).
fn find_restored_root(dest: &Path, sentinel: &str) -> PathBuf {
    for entry in walkdir::WalkDir::new(dest) {
        let entry = entry.expect("walk dest");
        if entry.file_type().is_file()
            && entry.file_name().to_string_lossy() == sentinel
        {
            return entry
                .path()
                .parent()
                .expect("sentinel must have a parent")
                .to_path_buf();
        }
    }
    panic!(
        "could not find sentinel `{sentinel}` anywhere under `{}`",
        dest.display()
    );
}

/// Assert two directory trees contain the exact same set of files with
/// the exact same byte content. Path-only mismatches and content-only
/// mismatches both fail with a precise message.
fn assert_trees_match(expected_root: &Path, actual_root: &Path) {
    let expected = snapshot_tree(expected_root);
    let actual = snapshot_tree(actual_root);

    let exp_paths: Vec<&Path> = expected.iter().map(|(p, _)| p.as_path()).collect();
    let act_paths: Vec<&Path> = actual.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(
        exp_paths, act_paths,
        "restored file listing differs from source:\n  source: {:?}\n  restored: {:?}",
        exp_paths, act_paths,
    );

    for ((p_exp, c_exp), (_p_act, c_act)) in expected.iter().zip(actual.iter()) {
        assert_eq!(
            c_exp.len(),
            c_act.len(),
            "size mismatch for `{}`: expected {} bytes, got {} bytes",
            p_exp.display(),
            c_exp.len(),
            c_act.len(),
        );
        assert_eq!(
            c_exp,
            c_act,
            "content mismatch for `{}` (sizes match but bytes differ)",
            p_exp.display(),
        );
    }
}

#[test]
fn restore_round_trip_rustic() {
    use kovre_core::backup::{engine_for, BackupSource};
    use kovre_core::config::{BackendKind, Repository as RepoConfig};

    let workspace = TempDir::new().unwrap();
    let root = workspace.path();
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    write_source_tree(&source, &restore_fixture_files());

    let repo_path = root.join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    let password_file = root.join("repo.key");
    fs::write(&password_file, "restore-test-pass\n").unwrap();

    let repo_cfg = RepoConfig {
        path: repo_path.clone(),
        backend: BackendKind::Rustic,
        password_file: Some(password_file.clone()),
    smb_user: None,
    smb_password_file: None,
    };

    let engine = engine_for(&repo_cfg);
    engine.init().expect("init rustic repo");
    engine
        .backup(
            "round-trip",
            BackupSource {
                paths: vec![source.clone()],
                excludes: vec![],
            },
        )
        .expect("backup");

    let dest = root.join("restored");
    engine_for(&repo_cfg)
        .restore_latest("round-trip", &dest)
        .expect("restore_latest");

    // rustic restores into a path that reflects the source's absolute
    // path on disk (drops the `:` from the drive letter on Windows).
    // Rather than hard-coding that convention, locate the restored
    // root by its sentinel file.
    let restored_root = find_restored_root(&dest, "hello.txt");
    assert_trees_match(&source, &restored_root);
}

#[test]
fn restore_round_trip_mirror() {
    use kovre_core::backup::{engine_for, BackupSource};
    use kovre_core::config::{BackendKind, Repository as RepoConfig};

    let workspace = TempDir::new().unwrap();
    let root = workspace.path();
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    write_source_tree(&source, &restore_fixture_files());

    let repo_path = root.join("repo");
    fs::create_dir_all(&repo_path).unwrap();

    let repo_cfg = RepoConfig {
        path: repo_path.clone(),
        backend: BackendKind::Mirror,
        password_file: None,
    smb_user: None,
    smb_password_file: None,
    };

    let engine = engine_for(&repo_cfg);
    engine.init().expect("init mirror repo");
    engine
        .backup(
            "round-trip",
            BackupSource {
                paths: vec![source.clone()],
                excludes: vec![],
            },
        )
        .expect("backup");

    let dest = root.join("restored");
    engine_for(&repo_cfg)
        .restore_latest("round-trip", &dest)
        .expect("restore_latest");

    // Mirror lays out the canonical files at `<dest>/<source
    // basename>/...` — locate it via the sentinel for symmetry with
    // the rustic test.
    let restored_root = find_restored_root(&dest, "hello.txt");
    assert_trees_match(&source, &restored_root);
}
