//! `thunderbird-mail` template — backs up every Thunderbird profile
//! stored under `%APPDATA%\Thunderbird\Profiles\`.
//!
//! What it covers: messages (Mail/, ImapMail/), address book
//! (abook.sqlite, history.sqlite), prefs (prefs.js, user.js),
//! attachments, calendar (calendar-data/). Everything Thunderbird
//! needs to recreate the user's exact state on a fresh install.
//!
//! What is excluded: cache directories (regenerable at startup) and
//! lock files (per-run, no semantic value).
//!
//! If Thunderbird isn't installed at all, resolution returns an
//! empty path list — caller treats that as "template empty on this
//! machine" rather than an error.

use std::path::PathBuf;

use anyhow::Result;

use super::{ResolvedTemplate, Template};

pub struct ThunderbirdMailTemplate;

impl ThunderbirdMailTemplate {
    pub const NAME: &'static str = "thunderbird-mail";

    const EXCLUDES: &[&str] = &[
        "**/cache2/**",
        "**/Cache/**",
        "**/startupCache/**",
        "**/lock",
        "**/parent.lock",
        "**/*.lock",
    ];
}

impl Template for ThunderbirdMailTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, _options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        let paths = resolve_paths_from_appdata(dirs::data_dir());
        Ok(ResolvedTemplate {
            paths,
            excludes: Self::EXCLUDES.iter().map(|s| s.to_string()).collect(),
        })
    }
}

/// Pure resolution given a base `%APPDATA%` candidate. Factored out
/// so tests can drive it with a TempDir instead of relying on the
/// running user's real profile.
fn resolve_paths_from_appdata(appdata: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(appdata) = appdata else {
        return Vec::new();
    };
    let profiles_root = appdata.join("Thunderbird").join("Profiles");
    if !profiles_root.is_dir() {
        return Vec::new();
    }

    let mut profiles: Vec<PathBuf> = match std::fs::read_dir(&profiles_root) {
        Ok(it) => it
            .filter_map(|res| res.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    // Deterministic ordering — easier to inspect on the inventory page.
    profiles.sort();
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn name_is_thunderbird_mail() {
        assert_eq!(ThunderbirdMailTemplate.name(), "thunderbird-mail");
    }

    #[test]
    fn empty_when_appdata_is_none() {
        let paths = resolve_paths_from_appdata(None);
        assert!(paths.is_empty());
    }

    #[test]
    fn empty_when_thunderbird_not_installed() {
        let temp = TempDir::new().unwrap();
        // No Thunderbird folder under the fake AppData.
        let paths = resolve_paths_from_appdata(Some(temp.path().to_path_buf()));
        assert!(paths.is_empty());
    }

    #[test]
    fn finds_every_profile_directory() {
        let temp = TempDir::new().unwrap();
        let profiles = temp.path().join("Thunderbird").join("Profiles");
        fs::create_dir_all(profiles.join("abcd1234.default-release")).unwrap();
        fs::create_dir_all(profiles.join("ef567890.work")).unwrap();
        // A regular file at the same level — should be ignored.
        fs::write(profiles.join("profiles.ini"), b"[General]\nStartWithLastProfile=1\n").unwrap();

        let paths = resolve_paths_from_appdata(Some(temp.path().to_path_buf()));
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("abcd1234.default-release")));
        assert!(paths.iter().any(|p| p.ends_with("ef567890.work")));
    }

    #[test]
    fn registry_lookup_returns_thunderbird_mail() {
        let resolved = super::super::resolve("thunderbird-mail", &serde_yaml::Value::Null);
        assert!(resolved.is_ok(), "registry lookup must succeed");
        let resolved = resolved.unwrap();
        // Excludes are always populated, paths may be empty on a
        // machine without Thunderbird.
        assert!(!resolved.excludes.is_empty());
    }

    #[test]
    fn excludes_include_cache_patterns() {
        let resolved = ThunderbirdMailTemplate
            .resolve(&serde_yaml::Value::Null)
            .unwrap();
        assert!(resolved.excludes.iter().any(|e| e.contains("cache2")));
        assert!(resolved.excludes.iter().any(|e| e.contains("Cache")));
    }
}
