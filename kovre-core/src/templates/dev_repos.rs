use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, warn};
use walkdir::WalkDir;

use super::{ResolvedTemplate, Template};

pub struct DevReposTemplate;

impl DevReposTemplate {
    pub const NAME: &'static str = "dev-repos";

    /// Directories that bloat backups but are trivially regeneratable from source.
    /// Skipped both at scan time (not descended into when searching for git roots)
    /// and at backup time (excluded from the snapshot contents).
    const SKIP_DIR_NAMES: &[&str] = &[
        "node_modules",
        "target",
        ".venv",
        "dist",
        "build",
        ".next",
    ];
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevReposOptions {
    /// Root directory to scan for git repositories. Defaults to the user's home directory.
    scan_root: Option<PathBuf>,
}

impl Template for DevReposTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        let opts: DevReposOptions = if options.is_null() {
            DevReposOptions::default()
        } else {
            serde_yaml::from_value(options.clone())
                .context("parsing template_options for dev-repos template")?
        };

        let scan_root = match opts.scan_root {
            Some(p) => p,
            None => dirs::home_dir().context(
                "could not locate the user's home directory (no `scan_root` provided)",
            )?,
        };

        let paths = scan_for_git_roots(&scan_root);

        let excludes: Vec<String> = Self::SKIP_DIR_NAMES
            .iter()
            .map(|name| format!("**/{name}"))
            .collect();

        Ok(ResolvedTemplate {
            paths,
            excludes,
            path_labels: std::collections::HashMap::new(),
        })
    }
}

/// Walk `scan_root` and return every directory that directly contains a `.git`
/// child (file or directory — git worktrees / submodules use a `.git` file).
///
/// Performance considerations:
/// - We `skip_current_dir` into found git roots (no point descending — the whole
///   repo will be backed up as a unit).
/// - We `skip_current_dir` into well-known build/cache directories so we don't
///   walk through massive `node_modules` trees while searching.
/// - I/O errors on individual entries are logged at WARN and skipped, not fatal —
///   `%USERPROFILE%` typically contains restricted folders (AppData\Packages, etc.).
fn scan_for_git_roots(scan_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !scan_root.exists() {
        warn!(scan_root = %scan_root.display(), "scan_root does not exist — skipping");
        return found;
    }

    let mut iter = WalkDir::new(scan_root).follow_links(false).into_iter();
    while let Some(entry) = iter.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                warn!(error = %err, "skipping unreadable entry while scanning for git roots");
                continue;
            }
        };

        if !entry.file_type().is_dir() {
            continue;
        }

        let name = entry.file_name();
        if DevReposTemplate::SKIP_DIR_NAMES
            .iter()
            .any(|s| OsStr::new(s) == name)
        {
            iter.skip_current_dir();
            continue;
        }

        if entry.path().join(".git").exists() {
            debug!(path = %entry.path().display(), "found git root");
            found.push(entry.path().to_path_buf());
            // Don't descend into this repo — we'll back up the whole thing.
            iter.skip_current_dir();
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn name_is_dev_repos() {
        assert_eq!(DevReposTemplate.name(), "dev-repos");
    }

    #[test]
    fn excludes_match_brief() {
        let resolved = DevReposTemplate
            .resolve(&serde_yaml::from_str("scan_root: /tmp/nonexistent-on-purpose").unwrap())
            .unwrap();
        assert_eq!(
            resolved.excludes,
            vec![
                "**/node_modules".to_string(),
                "**/target".to_string(),
                "**/.venv".to_string(),
                "**/dist".to_string(),
                "**/build".to_string(),
                "**/.next".to_string(),
            ]
        );
    }

    #[test]
    fn nonexistent_scan_root_returns_empty_paths() {
        let opts = serde_yaml::from_str(r"scan_root: /this/does/not/exist/anywhere").unwrap();
        let resolved = DevReposTemplate.resolve(&opts).unwrap();
        assert!(resolved.paths.is_empty());
    }

    #[test]
    fn rejects_unknown_template_option() {
        let opts: serde_yaml::Value =
            serde_yaml::from_str("scan_root: /tmp\nunknown_field: 1").unwrap();
        let err = DevReposTemplate.resolve(&opts).unwrap_err();
        assert!(
            err.to_string().contains("dev-repos") || err.chain().any(|e| e.to_string().contains("unknown")),
            "expected unknown-field error, got: {err:#}"
        );
    }

    #[test]
    fn finds_git_root_directly_under_scan_root() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("my-project");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join("README.md"), "x").unwrap();

        let opts = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                "scan_root".into(),
                serde_yaml::Value::String(dir.path().to_string_lossy().to_string()),
            );
            m
        });
        let resolved = DevReposTemplate.resolve(&opts).unwrap();
        assert_eq!(resolved.paths, vec![repo]);
    }

    #[test]
    fn does_not_descend_into_found_repo() {
        let dir = tempdir().unwrap();
        let outer = dir.path().join("outer");
        fs::create_dir_all(outer.join(".git")).unwrap();
        // Nested submodule-ish repo — should NOT be reported separately because we
        // stop descending once we found `outer`.
        let inner = outer.join("vendor").join("inner");
        fs::create_dir_all(inner.join(".git")).unwrap();

        let opts = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                "scan_root".into(),
                serde_yaml::Value::String(dir.path().to_string_lossy().to_string()),
            );
            m
        });
        let resolved = DevReposTemplate.resolve(&opts).unwrap();
        assert_eq!(resolved.paths, vec![outer]);
    }

    #[test]
    fn skips_excluded_dir_names_at_scan_time() {
        let dir = tempdir().unwrap();
        // Create a node_modules dir with a fake nested .git that should NOT be discovered.
        let trap = dir.path().join("node_modules").join("evil-dep");
        fs::create_dir_all(trap.join(".git")).unwrap();
        // Real repo elsewhere
        let real = dir.path().join("real-project");
        fs::create_dir_all(real.join(".git")).unwrap();

        let opts = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                "scan_root".into(),
                serde_yaml::Value::String(dir.path().to_string_lossy().to_string()),
            );
            m
        });
        let resolved = DevReposTemplate.resolve(&opts).unwrap();
        assert_eq!(resolved.paths, vec![real]);
    }

    #[test]
    fn finds_two_sibling_repos() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("alpha");
        let b = dir.path().join("beta");
        fs::create_dir_all(a.join(".git")).unwrap();
        fs::create_dir_all(b.join(".git")).unwrap();

        let opts = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                "scan_root".into(),
                serde_yaml::Value::String(dir.path().to_string_lossy().to_string()),
            );
            m
        });
        let mut resolved = DevReposTemplate.resolve(&opts).unwrap();
        resolved.paths.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(resolved.paths, expected);
    }

    #[test]
    fn registry_lookup_returns_dev_repos() {
        let opts = serde_yaml::from_str("scan_root: /tmp/nonexistent").unwrap();
        let resolved = super::super::resolve("dev-repos", &opts).unwrap();
        assert!(resolved.paths.is_empty());
    }
}
