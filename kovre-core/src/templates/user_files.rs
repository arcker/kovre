//! `user-files` template — extends the original `documents` to cover
//! all the personal known folders a Windows user typically cares
//! about: Documents, Desktop, Pictures, Music, Videos, Downloads, and
//! Saved Games. Each path is resolved via the Windows known-folders
//! API (`dirs::*`) so OneDrive redirection and folder relocation are
//! handled transparently.
//!
//! The original template name `documents` is kept as an alias in the
//! registry so existing `kovre.yaml` files continue to work without
//! migration.

use std::path::PathBuf;

use anyhow::Result;

use super::{ResolvedTemplate, Template};

pub struct UserFilesTemplate;

impl UserFilesTemplate {
    pub const NAME: &'static str = "user-files";

    const EXCLUDES: &[&str] = &["**/Thumbs.db", "**/*.tmp", "**/desktop.ini"];
}

impl Template for UserFilesTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, _options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        // Best-effort: skip silently any known folder the OS can't
        // resolve. A stripped-down Windows install may legitimately
        // miss `Saved Games` or `Videos`, and that shouldn't fail
        // the whole template.
        let mut paths: Vec<PathBuf> = Vec::new();

        for candidate in [
            dirs::document_dir(),
            dirs::desktop_dir(),
            dirs::picture_dir(),
            dirs::audio_dir(),    // Music
            dirs::video_dir(),    // Videos
            dirs::download_dir(), // Downloads
        ] {
            if let Some(p) = candidate {
                paths.push(p);
            }
        }

        // Saved Games — there is no `dirs` helper. The canonical
        // location is %USERPROFILE%\Saved Games, present since
        // Windows Vista, used by most modern game launchers.
        if let Some(home) = dirs::home_dir() {
            let sg = home.join("Saved Games");
            if sg.is_dir() {
                paths.push(sg);
            }
        }

        Ok(ResolvedTemplate {
            paths,
            excludes: Self::EXCLUDES.iter().map(|s| s.to_string()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_user_files() {
        assert_eq!(UserFilesTemplate.name(), "user-files");
        assert_eq!(UserFilesTemplate::NAME, "user-files");
    }

    #[test]
    fn resolves_at_least_documents_desktop_pictures() {
        // On any normal Windows test runner the three historical
        // known folders exist. Music/Videos/Downloads usually too;
        // Saved Games only if it has been created (a launcher
        // typically does that on first run). So we assert ≥3 paths
        // rather than ==7.
        let resolved = UserFilesTemplate
            .resolve(&serde_yaml::Value::Null)
            .expect("resolution must succeed on a normal Windows profile");

        assert!(
            resolved.paths.len() >= 3,
            "expected at least Documents/Desktop/Pictures, got {} paths",
            resolved.paths.len()
        );
        for p in &resolved.paths {
            assert!(p.is_absolute(), "path must be absolute: {p:?}");
        }

        assert_eq!(
            resolved.excludes,
            vec![
                "**/Thumbs.db".to_string(),
                "**/*.tmp".to_string(),
                "**/desktop.ini".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_arbitrary_options() {
        let opts: serde_yaml::Value = serde_yaml::from_str("foo: bar\nbaz: 42").unwrap();
        let resolved = UserFilesTemplate.resolve(&opts).unwrap();
        assert!(resolved.paths.len() >= 3);
    }

    #[test]
    fn documents_alias_resolves_to_user_files() {
        // Existing kovre.yaml files use `template: documents`; the
        // alias must keep working.
        let via_alias = super::super::resolve("documents", &serde_yaml::Value::Null).unwrap();
        let via_canonical =
            super::super::resolve("user-files", &serde_yaml::Value::Null).unwrap();
        assert_eq!(via_alias.paths, via_canonical.paths);
        assert_eq!(via_alias.excludes, via_canonical.excludes);
    }

    #[test]
    fn registry_rejects_unknown_template() {
        let err = super::super::resolve("ghost", &serde_yaml::Value::Null).unwrap_err();
        assert!(err.to_string().contains("unknown template"));
    }
}
