use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse YAML in `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("job `{job}` references unknown repository `{repository}`")]
    UnknownRepository { job: String, repository: String },
    #[error("job `{job}` must specify either `template` or `paths`")]
    JobMissingSource { job: String },
    #[error("job `{job}` cannot use both `template` and explicit `paths`/`excludes`")]
    JobTemplateAndPaths { job: String },
    #[error("repository `{repository}` uses the `rustic` backend but has no `password_file`")]
    RusticMissingPasswordFile { repository: String },
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub data_dir: PathBuf,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

/// Storage format kovre uses for a repository. Phase 4 introduces the
/// `Mirror` backend; `Rustic` stays the default for compat with all
/// configs written before Phase 4 landed.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    #[default]
    Rustic,
    Mirror,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub path: PathBuf,

    /// Engine that drives reads/writes against `path`. Defaults to
    /// `rustic` if omitted, which is what every Phase 1+2+3 config
    /// did implicitly.
    #[serde(default)]
    pub backend: BackendKind,

    /// Rustic-only: path to the passphrase file. Required for
    /// `BackendKind::Rustic`, ignored (and typically absent) for
    /// `BackendKind::Mirror`. Validation happens in `Config::validate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_last: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_hourly: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_daily: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_weekly: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_monthly: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_yearly: Option<u32>,

    /// Mirror backend only: how many archived copies of a file are kept
    /// in `.versions/<relpath>/`. Ignored by the rustic engine, which
    /// reads the `keep_*` snapshot fields above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_versions: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub repository: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_options: Option<serde_yaml::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<PathBuf>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excludes: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub agent: Agent,
    pub repositories: IndexMap<String, Repository>,
    pub jobs: IndexMap<String, Job>,
}

impl Config {
    pub fn from_str(yaml: &str, source_path: &Path) -> Result<Self, ConfigError> {
        let cfg: Config = serde_yaml::from_str(yaml).map_err(|source| ConfigError::Parse {
            path: source_path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let yaml = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_str(&yaml, path)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (name, repo) in &self.repositories {
            if repo.backend == BackendKind::Rustic && repo.password_file.is_none() {
                return Err(ConfigError::RusticMissingPasswordFile {
                    repository: name.clone(),
                });
            }
            // Mirror with an extra password_file is accepted on purpose
            // — it doesn't hurt anything to keep the file lying around,
            // and a user migrating a rustic repo to mirror might leave
            // the field behind without realising.
        }

        for (name, job) in &self.jobs {
            if !self.repositories.contains_key(&job.repository) {
                return Err(ConfigError::UnknownRepository {
                    job: name.clone(),
                    repository: job.repository.clone(),
                });
            }

            let has_template = job.template.is_some();
            let has_explicit_paths = job.paths.is_some();
            let has_explicit_excludes = job.excludes.is_some();

            if !has_template && !has_explicit_paths {
                return Err(ConfigError::JobMissingSource { job: name.clone() });
            }
            if has_template && (has_explicit_paths || has_explicit_excludes) {
                return Err(ConfigError::JobTemplateAndPaths { job: name.clone() });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_path() -> PathBuf {
        PathBuf::from("test.yaml")
    }

    #[test]
    fn parses_full_example() {
        let yaml = include_str!("../../kovre.example.yaml");
        let cfg = Config::from_str(yaml, &fake_path()).expect("example must parse");

        assert_eq!(cfg.agent.data_dir, PathBuf::from(r"C:\ProgramData\Kovre"));
        assert_eq!(cfg.agent.log_level, "info");

        assert_eq!(cfg.repositories.len(), 1);
        let nas = cfg.repositories.get("nas").unwrap();
        assert_eq!(nas.path, PathBuf::from(r"\\nas.local\backup\kovre"));
        assert_eq!(
            nas.password_file,
            Some(PathBuf::from(r"C:\ProgramData\Kovre\nas.key"))
        );

        assert_eq!(cfg.jobs.len(), 4);

        let order: Vec<&String> = cfg.jobs.keys().collect();
        assert_eq!(order, vec!["documents", "dev", "steam", "custom-photos"]);

        let documents = cfg.jobs.get("documents").unwrap();
        assert_eq!(documents.template.as_deref(), Some("documents"));
        assert_eq!(documents.repository, "nas");
        let ret = documents.retention.as_ref().unwrap();
        assert_eq!(ret.keep_daily, Some(7));
        assert_eq!(ret.keep_weekly, Some(4));
        assert_eq!(ret.keep_monthly, Some(12));

        let dev = cfg.jobs.get("dev").unwrap();
        assert_eq!(dev.template.as_deref(), Some("dev-repos"));
        let opts = dev.template_options.as_ref().unwrap();
        let scan_root = opts.get("scan_root").unwrap().as_str().unwrap();
        assert_eq!(scan_root, r"D:\dev");

        let custom = cfg.jobs.get("custom-photos").unwrap();
        assert!(custom.template.is_none());
        assert_eq!(
            custom.paths.as_ref().unwrap(),
            &vec![PathBuf::from(r"D:\Photos")]
        );
        assert_eq!(
            custom.excludes.as_ref().unwrap(),
            &vec!["**/*.tmp".to_string()]
        );
    }

    #[test]
    fn defaults_log_level_to_info() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  local:
    path: D:\backup
    password_file: D:\backup.key
jobs:
  docs:
    template: documents
    repository: local
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        assert_eq!(cfg.agent.log_level, "info");
    }

    #[test]
    fn rejects_unknown_repository() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: \\nas\share
    password_file: C:\nas.key
jobs:
  oops:
    template: documents
    repository: ghost
"#;
        let err = Config::from_str(yaml, &fake_path()).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownRepository { ref job, ref repository }
                if job == "oops" && repository == "ghost"),
            "expected UnknownRepository, got {err:?}"
        );
    }

    #[test]
    fn rejects_job_with_neither_template_nor_paths() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: \\nas\share
    password_file: C:\nas.key
jobs:
  empty:
    repository: nas
"#;
        let err = Config::from_str(yaml, &fake_path()).unwrap_err();
        assert!(matches!(err, ConfigError::JobMissingSource { ref job } if job == "empty"));
    }

    #[test]
    fn rejects_job_with_template_and_paths() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: \\nas\share
    password_file: C:\nas.key
jobs:
  mixed:
    template: documents
    repository: nas
    paths:
      - C:\foo
"#;
        let err = Config::from_str(yaml, &fake_path()).unwrap_err();
        assert!(matches!(err, ConfigError::JobTemplateAndPaths { ref job } if job == "mixed"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories: {}
jobs: {}
mystery_section: 42
"#;
        let err = Config::from_str(yaml, &fake_path()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_unc_and_drive_letter_paths() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas-unc:
    path: \\nas.local\backup\kovre
    password_file: C:\Kovre\nas.key
  local-drive:
    path: X:\backup
    password_file: X:\backup.key
jobs:
  docs:
    template: documents
    repository: local-drive
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        assert_eq!(
            cfg.repositories.get("nas-unc").unwrap().path,
            PathBuf::from(r"\\nas.local\backup\kovre")
        );
        assert_eq!(
            cfg.repositories.get("local-drive").unwrap().path,
            PathBuf::from(r"X:\backup")
        );
    }

    #[test]
    fn retention_is_optional() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: X:\backup
    password_file: X:\backup.key
jobs:
  docs:
    template: documents
    repository: nas
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        assert!(cfg.jobs.get("docs").unwrap().retention.is_none());
    }

    #[test]
    fn backend_defaults_to_rustic_when_omitted() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: X:\backup
    password_file: X:\backup.key
jobs: {}
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        assert_eq!(cfg.repositories["nas"].backend, BackendKind::Rustic);
        assert_eq!(
            cfg.repositories["nas"].password_file,
            Some(PathBuf::from(r"X:\backup.key"))
        );
    }

    #[test]
    fn mirror_backend_parses_without_password_file() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  photos:
    backend: mirror
    path: X:\photos-versions
jobs: {}
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        assert_eq!(cfg.repositories["photos"].backend, BackendKind::Mirror);
        assert_eq!(cfg.repositories["photos"].password_file, None);
    }

    #[test]
    fn rustic_without_password_file_is_rejected() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  nas:
    path: X:\backup
jobs: {}
"#;
        let err = Config::from_str(yaml, &fake_path()).unwrap_err();
        assert!(
            matches!(err, ConfigError::RusticMissingPasswordFile { ref repository } if repository == "nas"),
            "expected RusticMissingPasswordFile, got {err:?}"
        );
    }

    #[test]
    fn keep_versions_field_parses() {
        let yaml = r#"
agent:
  data_dir: C:\ProgramData\Kovre
repositories:
  photos:
    backend: mirror
    path: X:\photos-versions
jobs:
  family:
    repository: photos
    paths:
      - D:\Pictures
    retention:
      keep_versions: 10
"#;
        let cfg = Config::from_str(yaml, &fake_path()).unwrap();
        let r = cfg.jobs.get("family").unwrap().retention.as_ref().unwrap();
        assert_eq!(r.keep_versions, Some(10));
    }
}
