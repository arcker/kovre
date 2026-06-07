use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

pub mod browser_profiles;
pub mod dev_repos;
pub mod steam_saves;
pub mod thunderbird_mail;
pub mod user_appdata;
pub mod user_files;

pub use browser_profiles::BrowserProfilesTemplate;
pub use dev_repos::DevReposTemplate;
pub use steam_saves::SteamSavesTemplate;
pub use thunderbird_mail::ThunderbirdMailTemplate;
pub use user_appdata::UserAppdataTemplate;
pub use user_files::UserFilesTemplate;

use crate::config::Job;

#[derive(Debug, Clone, Default)]
pub struct ResolvedTemplate {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<String>,
    /// Optional human-friendly label per source path. The mirror
    /// engine uses these as the sub-directory under
    /// `<repo>/<job>/` instead of the path basename — which lets
    /// `steam-saves` group by game name instead of producing many
    /// colliding `remote/` and `Saves/` folders.
    pub path_labels: HashMap<PathBuf, String>,
}

pub trait Template {
    fn name(&self) -> &'static str;
    fn resolve(&self, options: &serde_yaml::Value) -> Result<ResolvedTemplate>;
}

/// Look up a template by name and resolve it with the given options.
///
/// `options` should be `Value::Null` when the YAML job has no
/// `template_options:` block.
///
/// Legacy alias: `"documents"` (the original Phase 1 name) resolves
/// to [`UserFilesTemplate`] (the Phase 5 expansion that adds
/// Music/Videos/Downloads/Saved Games to the same set). Existing
/// `kovre.yaml` files keep working without migration.
pub fn resolve(name: &str, options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
    let template: Box<dyn Template> = match name {
        // Aliased: keep accepting the old name forever.
        "documents" | UserFilesTemplate::NAME => Box::new(UserFilesTemplate),
        DevReposTemplate::NAME => Box::new(DevReposTemplate),
        SteamSavesTemplate::NAME => Box::new(SteamSavesTemplate),
        ThunderbirdMailTemplate::NAME => Box::new(ThunderbirdMailTemplate),
        BrowserProfilesTemplate::NAME => Box::new(BrowserProfilesTemplate),
        UserAppdataTemplate::NAME => Box::new(UserAppdataTemplate),
        _ => anyhow::bail!("unknown template `{name}`"),
    };
    template.resolve(options)
}

/// Resolve a `Job` to a concrete `(paths, excludes)` pair, whether it uses a template
/// or specifies them explicitly. Config validation (in `Config::validate`) guarantees
/// that exactly one of the two is set.
pub fn resolve_job(job: &Job) -> Result<ResolvedTemplate> {
    if let Some(template_name) = &job.template {
        let opts = job
            .template_options
            .clone()
            .unwrap_or(serde_yaml::Value::Null);
        return resolve(template_name, &opts);
    }
    Ok(ResolvedTemplate {
        paths: job.paths.clone().unwrap_or_default(),
        excludes: job.excludes.clone().unwrap_or_default(),
        path_labels: HashMap::new(),
    })
}
