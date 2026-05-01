use anyhow::{Context, Result};

use super::{ResolvedTemplate, Template};

pub struct DocumentsTemplate;

impl DocumentsTemplate {
    pub const NAME: &'static str = "documents";

    const EXCLUDES: &[&str] = &["**/Thumbs.db", "**/*.tmp", "**/desktop.ini"];
}

impl Template for DocumentsTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, _options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        // Resolve via Windows known-folders API rather than `%USERPROFILE%\Documents` —
        // OneDrive redirection and folder relocation move these out from under the profile dir.
        let documents = dirs::document_dir()
            .context("could not locate the Documents folder (Windows known-folder lookup failed)")?;
        let desktop = dirs::desktop_dir()
            .context("could not locate the Desktop folder (Windows known-folder lookup failed)")?;
        let pictures = dirs::picture_dir()
            .context("could not locate the Pictures folder (Windows known-folder lookup failed)")?;

        Ok(ResolvedTemplate {
            paths: vec![documents, desktop, pictures],
            excludes: Self::EXCLUDES.iter().map(|s| s.to_string()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_documents() {
        assert_eq!(DocumentsTemplate.name(), "documents");
        assert_eq!(DocumentsTemplate::NAME, "documents");
    }

    #[test]
    fn resolves_three_paths_and_known_excludes() {
        let resolved = DocumentsTemplate
            .resolve(&serde_yaml::Value::Null)
            .expect("resolution must succeed on a normal Windows profile");

        assert_eq!(resolved.paths.len(), 3, "expected Documents, Desktop, Pictures");
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
        // documents has no options, but extra fields shouldn't break resolution.
        let resolved = DocumentsTemplate.resolve(&opts).unwrap();
        assert_eq!(resolved.paths.len(), 3);
    }

    #[test]
    fn registry_lookup_returns_documents() {
        let resolved = super::super::resolve("documents", &serde_yaml::Value::Null).unwrap();
        assert_eq!(resolved.paths.len(), 3);
    }

    #[test]
    fn registry_rejects_unknown_template() {
        let err = super::super::resolve("ghost", &serde_yaml::Value::Null).unwrap_err();
        assert!(err.to_string().contains("unknown template"));
    }
}
