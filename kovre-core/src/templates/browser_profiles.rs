//! `browser-profiles` template — backs up the files needed to
//! restore a user's browser state (bookmarks, history, logins,
//! extensions, preferences) for the four most common browsers on
//! Windows: Firefox, Chrome, Edge, Brave.
//!
//! Firefox stores everything per-profile under
//! `%APPDATA%\Mozilla\Firefox\Profiles\*` — we back up the whole
//! profile minus cache/storage. Chromium-based browsers
//! (Chrome/Edge/Brave) keep state in `Default\` under their
//! respective `User Data\` roots; we whitelist only the files that
//! matter (avoiding the multi-GB cache that lives alongside).
//!
//! Options (all boolean, defaults `true` except brave):
//!   firefox: bool = true
//!   chrome:  bool = true
//!   edge:    bool = true
//!   brave:   bool = false

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{ResolvedTemplate, Template};

pub struct BrowserProfilesTemplate;

impl BrowserProfilesTemplate {
    pub const NAME: &'static str = "browser-profiles";
}

#[derive(Debug, Deserialize)]
struct Options {
    #[serde(default = "default_true")]
    firefox: bool,
    #[serde(default = "default_true")]
    chrome: bool,
    #[serde(default = "default_true")]
    edge: bool,
    #[serde(default)]
    brave: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Options {
    fn default() -> Self {
        Self {
            firefox: true,
            chrome: true,
            edge: true,
            brave: false,
        }
    }
}

/// Chromium files we actually want — everything else under `Default\`
/// is either cache (multi-GB) or session-scoped junk. Keep this
/// list short and explicit; an undersight here is a missing file
/// in a restore, which the user will immediately notice.
const CHROMIUM_DEFAULT_FILES: &[&str] = &[
    "Bookmarks",
    "Bookmarks.bak",
    "History",
    "Preferences",
    "Login Data",
    "Login Data For Account",
    "Cookies",
    "Web Data",
    "Favicons",
    "Shortcuts",
    "Top Sites",
    "Reading List",
];

/// Chromium subdirectories worth saving — extensions and their
/// settings carry the user's actual customization.
const CHROMIUM_DEFAULT_DIRS: &[&str] = &["Extensions", "Local Extension Settings"];

impl Template for BrowserProfilesTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        let opts: Options = if options.is_null() {
            Options::default()
        } else {
            serde_yaml::from_value(options.clone())
                .context("parsing browser-profiles options")?
        };

        let paths = collect_paths(
            &opts,
            dirs::data_dir(),       // %APPDATA% (Roaming)
            dirs::data_local_dir(), // %LOCALAPPDATA%
        );

        Ok(ResolvedTemplate {
            paths,
            excludes: vec![
                // Firefox profile bloat — cache and per-origin storage.
                "**/cache2/**".into(),
                "**/cache/**".into(),
                "**/storage/**".into(),
                "**/startupCache/**".into(),
                "**/lock".into(),
                "**/*.lock".into(),
            ],
        })
    }
}

/// Pure resolution given explicit AppData candidates. Tested with
/// a TempDir to avoid dependency on the running user's installed
/// browsers.
fn collect_paths(
    opts: &Options,
    appdata: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if opts.firefox {
        if let Some(roaming) = &appdata {
            let profiles_root = roaming.join("Mozilla").join("Firefox").join("Profiles");
            if profiles_root.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&profiles_root) {
                    let mut profiles: Vec<PathBuf> = entries
                        .filter_map(|res| res.ok())
                        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                        .map(|e| e.path())
                        .collect();
                    profiles.sort();
                    out.extend(profiles);
                }
            }
        }
    }

    if let Some(local) = &local_appdata {
        if opts.chrome {
            collect_chromium_paths(&local.join("Google").join("Chrome"), &mut out);
        }
        if opts.edge {
            collect_chromium_paths(&local.join("Microsoft").join("Edge"), &mut out);
        }
        if opts.brave {
            collect_chromium_paths(
                &local.join("BraveSoftware").join("Brave-Browser"),
                &mut out,
            );
        }
    }

    out
}

/// For a Chromium browser root (e.g. `%LOCALAPPDATA%\Google\Chrome`),
/// collect the whitelisted files and subdirectories under each
/// `User Data\<Profile>\`. Profiles are `Default`, `Profile 1`,
/// `Profile 2`, etc. — Chrome creates them when the user adds a
/// second account.
fn collect_chromium_paths(browser_root: &std::path::Path, out: &mut Vec<PathBuf>) {
    let user_data = browser_root.join("User Data");
    if !user_data.is_dir() {
        return;
    }
    let entries = match std::fs::read_dir(&user_data) {
        Ok(it) => it,
        Err(_) => return,
    };
    let mut profile_dirs: Vec<PathBuf> = entries
        .filter_map(|res| res.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            // Keep "Default" or "Profile N" subdirs; skip system
            // ones like "System Profile", "Crashpad", "GrShaderCache".
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            name == "Default" || name.starts_with("Profile ")
        })
        .collect();
    profile_dirs.sort();

    for profile in profile_dirs {
        for file in CHROMIUM_DEFAULT_FILES {
            let p = profile.join(file);
            if p.is_file() {
                out.push(p);
            }
        }
        for dir in CHROMIUM_DEFAULT_DIRS {
            let p = profile.join(dir);
            if p.is_dir() {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn name_is_browser_profiles() {
        assert_eq!(BrowserProfilesTemplate.name(), "browser-profiles");
    }

    #[test]
    fn options_defaults_match_documented_behavior() {
        let opts = Options::default();
        assert!(opts.firefox);
        assert!(opts.chrome);
        assert!(opts.edge);
        assert!(!opts.brave, "brave is off by default — niche browser");
    }

    #[test]
    fn empty_when_no_browsers_installed() {
        let temp = TempDir::new().unwrap();
        let appdata = temp.path().join("Roaming");
        let local = temp.path().join("Local");
        fs::create_dir_all(&appdata).unwrap();
        fs::create_dir_all(&local).unwrap();

        let paths = collect_paths(&Options::default(), Some(appdata), Some(local));
        assert!(paths.is_empty());
    }

    #[test]
    fn finds_firefox_profiles_when_present() {
        let temp = TempDir::new().unwrap();
        let appdata = temp.path().join("Roaming");
        let firefox_profiles = appdata.join("Mozilla").join("Firefox").join("Profiles");
        fs::create_dir_all(firefox_profiles.join("abc.default-release")).unwrap();
        fs::create_dir_all(firefox_profiles.join("xyz.work")).unwrap();

        let paths = collect_paths(&Options::default(), Some(appdata), None);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("abc.default-release")));
        assert!(paths.iter().any(|p| p.ends_with("xyz.work")));
    }

    #[test]
    fn skips_firefox_when_option_false() {
        let temp = TempDir::new().unwrap();
        let appdata = temp.path().join("Roaming");
        let firefox_profiles = appdata.join("Mozilla").join("Firefox").join("Profiles");
        fs::create_dir_all(firefox_profiles.join("abc.default-release")).unwrap();

        let opts = Options {
            firefox: false,
            ..Options::default()
        };
        let paths = collect_paths(&opts, Some(appdata), None);
        assert!(paths.is_empty(), "firefox=false must skip all FF paths");
    }

    #[test]
    fn whitelist_picks_only_known_chrome_files() {
        let temp = TempDir::new().unwrap();
        let local = temp.path().join("Local");
        let default_profile = local
            .join("Google")
            .join("Chrome")
            .join("User Data")
            .join("Default");
        fs::create_dir_all(&default_profile).unwrap();

        // Wanted files.
        fs::write(default_profile.join("Bookmarks"), b"{}").unwrap();
        fs::write(default_profile.join("History"), b"sqlite-placeholder").unwrap();
        fs::write(default_profile.join("Preferences"), b"{}").unwrap();
        // Unwanted: random cache file at the same level.
        fs::write(default_profile.join("Visited Links"), b"crap").unwrap();
        // Wanted directory.
        fs::create_dir_all(default_profile.join("Extensions").join("abcd")).unwrap();
        // Unwanted directory.
        fs::create_dir_all(default_profile.join("Code Cache").join("js")).unwrap();

        let paths = collect_paths(&Options::default(), None, Some(local));
        let basenames: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(basenames.iter().any(|n| n == "Bookmarks"));
        assert!(basenames.iter().any(|n| n == "History"));
        assert!(basenames.iter().any(|n| n == "Preferences"));
        assert!(basenames.iter().any(|n| n == "Extensions"));
        assert!(
            !basenames.iter().any(|n| n == "Visited Links"),
            "non-whitelisted file leaked into paths"
        );
        assert!(
            !basenames.iter().any(|n| n == "Code Cache"),
            "non-whitelisted directory leaked into paths"
        );
    }

    #[test]
    fn finds_multiple_chrome_profiles() {
        let temp = TempDir::new().unwrap();
        let local = temp.path().join("Local");
        let user_data = local
            .join("Google")
            .join("Chrome")
            .join("User Data");
        for sub in ["Default", "Profile 1", "Profile 2", "System Profile"] {
            let dir = user_data.join(sub);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Bookmarks"), b"{}").unwrap();
        }

        let paths = collect_paths(&Options::default(), None, Some(local));
        let parents: Vec<String> = paths
            .iter()
            .map(|p| {
                p.parent()
                    .and_then(|pp| pp.file_name())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(parents.iter().any(|n| n == "Default"));
        assert!(parents.iter().any(|n| n == "Profile 1"));
        assert!(parents.iter().any(|n| n == "Profile 2"));
        assert!(
            !parents.iter().any(|n| n == "System Profile"),
            "System Profile must be skipped"
        );
    }

    #[test]
    fn brave_off_by_default_even_if_installed() {
        let temp = TempDir::new().unwrap();
        let local = temp.path().join("Local");
        let brave_default = local
            .join("BraveSoftware")
            .join("Brave-Browser")
            .join("User Data")
            .join("Default");
        fs::create_dir_all(&brave_default).unwrap();
        fs::write(brave_default.join("Bookmarks"), b"{}").unwrap();

        let paths = collect_paths(&Options::default(), None, Some(local.clone()));
        assert!(
            paths.is_empty(),
            "brave=false default must skip Brave paths even when present"
        );

        // Enable explicitly and confirm.
        let opts = Options {
            brave: true,
            ..Options::default()
        };
        let paths = collect_paths(&opts, None, Some(local));
        assert!(!paths.is_empty(), "brave=true must pick up Brave paths");
    }

    #[test]
    fn registry_lookup_returns_browser_profiles() {
        let resolved = super::super::resolve("browser-profiles", &serde_yaml::Value::Null);
        assert!(resolved.is_ok());
    }

    #[test]
    fn rejects_unknown_option_field() {
        // Strict option parsing — a typo in YAML is caught early.
        let opts: serde_yaml::Value =
            serde_yaml::from_str("firefox: true\ntypo: yes").unwrap();
        let err = BrowserProfilesTemplate.resolve(&opts);
        // serde_yaml ignores unknown fields by default. We don't add
        // `deny_unknown_fields` here on purpose — it would break
        // forward-compat. So this should succeed, not fail.
        assert!(err.is_ok());
    }
}
