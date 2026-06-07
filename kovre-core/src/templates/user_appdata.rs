//! `user-appdata` template — catch-all over `%APPDATA%` (Roaming).
//!
//! Intended as a **safety net** rather than a curated backup: it
//! sweeps the Roaming AppData root and excludes the loud known
//! offenders (caches, logs, temp), plus the parts already covered by
//! more specific templates (Thunderbird, Firefox) so the user doesn't
//! pay storage twice when both jobs run against the same repo.
//!
//! No options — keep this simple. A user who wants finer control
//! writes a `custom` job with their own paths.

use std::path::PathBuf;

use anyhow::Result;

use super::{ResolvedTemplate, Template};

pub struct UserAppdataTemplate;

impl UserAppdataTemplate {
    pub const NAME: &'static str = "user-appdata";

    /// Excludes apply to relative paths from the source root, with
    /// forward-slash normalization (handled by the mirror/rustic
    /// engine). We use `**/` prefixes so the pattern matches at any
    /// depth.
    const EXCLUDES: &[&str] = &[
        // Generic noise.
        "**/Cache/**",
        "**/Cache2/**",
        "**/cache/**",
        "**/cache2/**",
        "**/Caches/**",
        "**/Temp/**",
        "**/tmp/**",
        "**/Logs/**",
        "**/logs/**",
        "**/CrashDumps/**",
        "**/CrashReports/**",
        // Covered by more specific templates — avoid double backup
        // when the user has both `thunderbird-mail` and
        // `browser-profiles` jobs alongside `user-appdata`.
        "**/Thunderbird/**",
        "**/Mozilla/Firefox/**",
        // Microsoft cache zones inside Roaming.
        "**/Microsoft/Windows/WebCache/**",
        "**/Microsoft/Windows/INetCache/**",
        "**/Microsoft/Windows/Recent/**",
    ];
}

impl Template for UserAppdataTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, _options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        let paths: Vec<PathBuf> = match dirs::data_dir() {
            Some(appdata) if appdata.is_dir() => vec![appdata],
            _ => Vec::new(),
        };
        Ok(ResolvedTemplate {
            paths,
            excludes: Self::EXCLUDES.iter().map(|s| s.to_string()).collect(),
            path_labels: std::collections::HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_user_appdata() {
        assert_eq!(UserAppdataTemplate.name(), "user-appdata");
    }

    #[test]
    fn resolves_one_path_pointing_at_appdata() {
        // On any normal Windows test runner %APPDATA% exists and
        // resolves to `C:\Users\<u>\AppData\Roaming`.
        let resolved = UserAppdataTemplate
            .resolve(&serde_yaml::Value::Null)
            .unwrap();
        assert!(
            resolved.paths.len() <= 1,
            "user-appdata should yield at most one path"
        );
        if let Some(p) = resolved.paths.first() {
            assert!(p.is_absolute());
            // Cheap sanity check on Windows runners.
            let s = p.to_string_lossy().to_lowercase();
            assert!(s.contains("roaming") || s.contains("appdata"));
        }
    }

    #[test]
    fn excludes_cover_well_known_offenders() {
        let resolved = UserAppdataTemplate
            .resolve(&serde_yaml::Value::Null)
            .unwrap();
        let blob = resolved.excludes.join(" | ");
        for needle in ["Cache", "Temp", "Logs", "Thunderbird", "Firefox"] {
            assert!(
                blob.contains(needle),
                "expected exclude pattern containing `{needle}`, got: {blob}"
            );
        }
    }

    #[test]
    fn registry_lookup_returns_user_appdata() {
        let resolved = super::super::resolve("user-appdata", &serde_yaml::Value::Null);
        assert!(resolved.is_ok());
    }
}
