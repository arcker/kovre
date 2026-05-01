use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, info, warn};
use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

use super::{ResolvedTemplate, Template};
use crate::ludusavi;

pub struct SteamSavesTemplate;

impl SteamSavesTemplate {
    pub const NAME: &'static str = "steam-saves";
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SteamSavesOptions {
    /// Override Steam install path. Defaults to registry detection
    /// (`HKLM\SOFTWARE\WOW6432Node\Valve\Steam` → `InstallPath`).
    steam_path: Option<PathBuf>,

    /// If true, use only the cached manifest, never the network.
    #[serde(default)]
    offline: bool,

    /// Extra Steam library paths in addition to the ones discovered via
    /// `libraryfolders.vdf`. Useful if a user's library file is malformed.
    #[serde(default)]
    extra_libraries: Vec<PathBuf>,
}

impl Template for SteamSavesTemplate {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn resolve(&self, options: &serde_yaml::Value) -> Result<ResolvedTemplate> {
        let opts: SteamSavesOptions = if options.is_null() {
            SteamSavesOptions::default()
        } else {
            serde_yaml::from_value(options.clone())
                .context("parsing template_options for steam-saves template")?
        };

        let steam_path = opts.steam_path.or_else(detect_steam_install);
        let Some(steam_path) = steam_path else {
            warn!(
                "Steam not detected (no `HKLM\\SOFTWARE\\Valve\\Steam\\InstallPath`); \
                 steam-saves template returns no paths"
            );
            return Ok(ResolvedTemplate {
                paths: Vec::new(),
                excludes: Vec::new(),
            });
        };
        info!(steam_path = %steam_path.display(), "Steam install detected");

        let mut libraries: Vec<PathBuf> = vec![steam_path.clone()];
        match find_extra_libraries(&steam_path) {
            Ok(extras) => libraries.extend(extras),
            Err(e) => warn!("could not parse libraryfolders.vdf: {e}"),
        }
        libraries.extend(opts.extra_libraries);
        libraries.sort();
        libraries.dedup();

        let installed = enumerate_installed_games(&libraries);
        info!(
            libraries = libraries.len(),
            games = installed.len(),
            "Steam libraries scanned"
        );

        let manifest = match ludusavi::ensure_manifest_blocking(!opts.offline) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    "Ludusavi manifest unavailable ({e}); steam-saves template returns no paths"
                );
                return Ok(ResolvedTemplate {
                    paths: Vec::new(),
                    excludes: Vec::new(),
                });
            }
        };

        let mut paths: Vec<PathBuf> = Vec::new();
        let mut games_matched = 0usize;
        let mut paths_skipped_unsupported = 0usize;

        for (game_name, spec) in &manifest {
            let install_match = spec.install_dir.keys().any(|k| installed.contains(k.as_str()));
            if !install_match {
                continue;
            }
            games_matched += 1;

            for (raw_path, file_spec) in &spec.files {
                if !file_spec.tags.iter().any(|t| t == "save") {
                    continue;
                }
                if !applies_to_windows(file_spec) {
                    continue;
                }
                match resolve_placeholders(raw_path) {
                    Some(p) => paths.push(p),
                    None => {
                        debug!(game = game_name, raw = raw_path, "unsupported placeholder");
                        paths_skipped_unsupported += 1;
                    }
                }
            }
        }

        paths.sort();
        paths.dedup();

        info!(
            games_matched,
            save_paths = paths.len(),
            paths_skipped_unsupported,
            "Steam saves resolved"
        );

        Ok(ResolvedTemplate {
            paths,
            excludes: Vec::new(),
        })
    }
}

fn detect_steam_install() -> Option<PathBuf> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    // Steam is a 32-bit process: on a 64-bit Windows, its registry keys live
    // under `WOW6432Node`. Try that first, then fall back to the native view
    // (which Steam writes to on 32-bit Windows installs).
    for subkey in [
        r"SOFTWARE\WOW6432Node\Valve\Steam",
        r"SOFTWARE\Valve\Steam",
    ] {
        if let Ok(key) = hklm.open_subkey(subkey) {
            if let Ok(path) = key.get_value::<String, _>("InstallPath") {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// Parse `<steam>/steamapps/libraryfolders.vdf` and return any library path
/// that is not the main Steam install dir.
fn find_extra_libraries(steam_path: &Path) -> Result<Vec<PathBuf>> {
    let vdf = steam_path.join("steamapps").join("libraryfolders.vdf");
    let content = fs::read_to_string(&vdf)
        .with_context(|| format!("reading `{}`", vdf.display()))?;

    let mut paths = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        // We only need the `"path"` lines; skip everything else.
        if let Some(rest) = line.strip_prefix("\"path\"") {
            if let Some(raw) = extract_quoted_value(rest) {
                // VDF escapes backslashes as `\\`; un-escape.
                let path = PathBuf::from(raw.replace("\\\\", "\\"));
                if path != *steam_path {
                    paths.push(path);
                }
            }
        }
    }
    Ok(paths)
}

fn extract_quoted_value(s: &str) -> Option<String> {
    let s = s.trim();
    let start = s.find('"')?;
    let after = &s[start + 1..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn enumerate_installed_games(libraries: &[PathBuf]) -> HashSet<String> {
    let mut installed = HashSet::new();
    for lib in libraries {
        let common = lib.join("steamapps").join("common");
        match fs::read_dir(&common) {
            Ok(entries) => {
                for e in entries.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        if let Some(n) = e.file_name().to_str() {
                            installed.insert(n.to_string());
                        }
                    }
                }
            }
            Err(e) => debug!(library = %common.display(), error = %e, "no common dir"),
        }
    }
    installed
}

fn applies_to_windows(spec: &ludusavi::FileSpec) -> bool {
    if spec.when.is_empty() {
        return true;
    }
    // If any `when` clause matches Windows (or is OS-agnostic), include.
    spec.when.iter().any(|w| match w.os.as_deref() {
        None => true,
        Some("windows") => true,
        _ => false,
    })
}

/// Substitute the Ludusavi placeholders we know how to handle on Windows.
/// Returns `None` if any unsupported placeholder remains after substitution.
///
/// Manifest paths sometimes contain glob characters (e.g. `…/SaveGames/*.sav`).
/// We can't pass globs to rustic as source paths, so we truncate to the parent
/// directory of the first glob-containing component. The caller will then back
/// up the whole directory — slightly more than just matching files, but
/// strictly a superset and harmless.
fn resolve_placeholders(raw: &str) -> Option<PathBuf> {
    let mut s = raw.to_string();

    let subs: &[(&str, fn() -> Option<PathBuf>)] = &[
        ("<home>", dirs::home_dir),
        ("<winDocuments>", dirs::document_dir),
        ("<winAppData>", dirs::config_dir), // %APPDATA% (Roaming) on Windows
        ("<winLocalAppData>", dirs::data_local_dir),
        ("<winPublic>", winpublic),
    ];

    for (placeholder, getter) in subs {
        if s.contains(placeholder) {
            let path = getter()?;
            s = s.replace(placeholder, &path.to_string_lossy());
        }
    }

    // Any `<...>` remaining → unsupported placeholder, give up.
    if s.contains('<') && s.contains('>') {
        return None;
    }

    let path = PathBuf::from(&s);
    let pruned = strip_glob_tail(&path);
    if pruned.as_os_str().is_empty() {
        return None;
    }
    Some(pruned)
}

/// Walk path components and stop at the first one that contains a glob
/// metachar (`*`, `?`, `[`). Returns the prefix up to (but not including) that
/// component.
fn strip_glob_tail(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy();
        if s.contains('*') || s.contains('?') || s.contains('[') {
            break;
        }
        out.push(comp);
    }
    out
}

fn winpublic() -> Option<PathBuf> {
    std::env::var_os("PUBLIC")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(r"C:\Users\Public")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_steam_saves() {
        assert_eq!(SteamSavesTemplate.name(), "steam-saves");
    }

    #[test]
    fn rejects_unknown_template_option() {
        let opts: serde_yaml::Value = serde_yaml::from_str("garbage: 1").unwrap();
        let err = SteamSavesTemplate.resolve(&opts).unwrap_err();
        assert!(err.to_string().contains("steam-saves"));
    }

    #[test]
    fn applies_to_windows_with_no_when_clause_returns_true() {
        let spec = ludusavi::FileSpec::default();
        assert!(applies_to_windows(&spec));
    }

    #[test]
    fn applies_to_windows_filter_matches() {
        let spec = ludusavi::FileSpec {
            tags: vec![],
            when: vec![ludusavi::WhenSpec {
                os: Some("windows".into()),
                store: None,
            }],
        };
        assert!(applies_to_windows(&spec));
    }

    #[test]
    fn applies_to_windows_excludes_other_os_only() {
        let spec = ludusavi::FileSpec {
            tags: vec![],
            when: vec![ludusavi::WhenSpec {
                os: Some("mac".into()),
                store: None,
            }],
        };
        assert!(!applies_to_windows(&spec));
    }

    #[test]
    fn resolve_placeholders_substitutes_winappdata() {
        // We can't predict the exact path, but the substitution should at
        // least produce an absolute path with no `<>` markers left.
        let resolved = resolve_placeholders("<winAppData>/MyGame/Saves").unwrap();
        let s = resolved.to_string_lossy();
        assert!(!s.contains('<'));
        assert!(!s.contains('>'));
        assert!(s.ends_with(r"MyGame\Saves") || s.ends_with("MyGame/Saves"));
    }

    #[test]
    fn resolve_placeholders_returns_none_for_unsupported() {
        // <storeUserId> is not in our supported set.
        assert!(resolve_placeholders("<storeUserId>/foo").is_none());
        assert!(resolve_placeholders("<base>/save.dat").is_none());
    }

    #[test]
    fn resolve_placeholders_passthrough_for_literal_path() {
        let r = resolve_placeholders("C:/literal/path").unwrap();
        assert_eq!(r, PathBuf::from("C:/literal/path"));
    }

    #[test]
    fn resolve_placeholders_strips_trailing_glob() {
        let r = resolve_placeholders("<winLocalAppData>/MyGame/Saves/*.sav").unwrap();
        let s = r.to_string_lossy();
        assert!(s.ends_with("MyGame\\Saves") || s.ends_with("MyGame/Saves"));
        assert!(!s.contains('*'));
    }

    #[test]
    fn strip_glob_tail_handles_intermediate_glob() {
        let p = PathBuf::from(r"C:\foo\*.dir\bar");
        assert_eq!(strip_glob_tail(&p), PathBuf::from(r"C:\foo"));
    }

    #[test]
    fn strip_glob_tail_handles_mixed_separators() {
        // This mirrors the exact path that came out of the real smoke test —
        // %LOCALAPPDATA% (back-slashes) followed by manifest path (forward slashes).
        let p = PathBuf::from(r"C:\Users\yoan\AppData\Local/Robot Entertainment/Orcs/SaveGames/*.sav");
        let stripped = strip_glob_tail(&p);
        let s = stripped.to_string_lossy();
        assert!(!s.contains('*'), "expected glob stripped, got {s:?}");
        assert!(s.contains("SaveGames"), "expected SaveGames preserved, got {s:?}");
    }

    #[test]
    fn extract_quoted_value_handles_typical_vdf_line() {
        // Mirrors a real `"path"\t\t"D:\\SteamLibrary"` line.
        let raw = r#"		"D:\\SteamLibrary""#;
        assert_eq!(extract_quoted_value(raw).as_deref(), Some(r"D:\\SteamLibrary"));
    }
}
