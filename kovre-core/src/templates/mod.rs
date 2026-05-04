use std::path::PathBuf;

use anyhow::Result;

pub mod dev_repos;
pub mod documents;
pub mod steam_saves;

pub use dev_repos::DevReposTemplate;
pub use documents::DocumentsTemplate;
pub use steam_saves::SteamSavesTemplate;

use crate::config::Job;

#[derive(Debug, Clone)]
pub struct ResolvedTemplate {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<String>,
}

pub trait Template {
    fn name(&self) -> &'static str;
    fn resolve(&self, options: &serde_yaml::Value) -> Result<ResolvedTemplate>;
}

/// Look up a template by name and resolve it with the given options.
///
/// `options` should be `Value::Null` when the YAML job has no `template_options:` block.
pub fn resolve(name: &str, options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
    let template: Box<dyn Template> = match name {
        DocumentsTemplate::NAME => Box::new(DocumentsTemplate),
        DevReposTemplate::NAME => Box::new(DevReposTemplate),
        SteamSavesTemplate::NAME => Box::new(SteamSavesTemplate),
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
    })
}
