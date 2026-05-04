//! Download / cache / parse the Ludusavi community manifest.
//!
//! The manifest (`manifest.yaml` upstream) is a ~17 MB YAML mapping
//! `GameName -> GameSpec`. We use it to resolve Steam save-game locations.
//!
//! Cache lives under `dirs::cache_dir()/kovre/ludusavi/` and uses ETag
//! conditional GET to avoid re-downloading unchanged manifests.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::IgnoredAny;
use tracing::{info, warn};

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

/// The full Ludusavi manifest.
pub type Manifest = BTreeMap<String, GameSpec>;

/// One game's entry in the manifest. Only the fields we use are typed —
/// everything else is silently ignored by serde (no `deny_unknown_fields`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GameSpec {
    /// Names of the folder under `steamapps/common/` (typically one entry).
    pub install_dir: BTreeMap<String, IgnoredAny>,

    /// Path patterns where saves/configs/etc. live, with placeholders.
    pub files: BTreeMap<String, FileSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileSpec {
    pub tags: Vec<String>,
    pub when: Vec<WhenSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WhenSpec {
    pub os: Option<String>,
    pub store: Option<String>,
}

/// Default cache directory: `<user-cache-dir>/kovre/ludusavi/`.
pub fn default_cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("no user cache dir available on this platform")?;
    Ok(base.join("kovre").join("ludusavi"))
}

/// Ensure a manifest is available and return it parsed.
///
/// Online mode (default): attempts an ETag-conditional GET. If the network is
/// unreachable, falls back to the cache.
/// Offline mode: only uses the cache.
///
/// Returns an error only if no manifest can be obtained from any source.
pub fn ensure_manifest_blocking(online: bool) -> Result<Manifest> {
    let cache = default_cache_dir()?;
    ensure_manifest_blocking_with(MANIFEST_URL, &cache, online)
}

/// Like `ensure_manifest_blocking` but with an explicit upstream URL and cache
/// directory. Exposed for tests, where we serve the manifest from a local mock
/// server and a `TempDir` cache instead of touching the user's real cache.
pub fn ensure_manifest_blocking_with(
    url: &str,
    cache: &Path,
    online: bool,
) -> Result<Manifest> {
    fs::create_dir_all(cache)
        .with_context(|| format!("creating cache dir `{}`", cache.display()))?;

    let yaml_path = cache.join("manifest.yaml");
    let etag_path = cache.join("manifest.etag");

    if !online {
        return load_cached(&yaml_path)
            .with_context(|| format!("offline mode and cache unreadable at `{}`", yaml_path.display()));
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating tokio runtime for manifest download")?;

    rt.block_on(fetch_or_load(url, &yaml_path, &etag_path))
}

async fn fetch_or_load(url: &str, yaml_path: &Path, etag_path: &Path) -> Result<Manifest> {
    let cached_etag = fs::read_to_string(etag_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let client = match reqwest::Client::builder()
        .user_agent(concat!("kovre/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to build HTTP client: {e}; trying cache");
            return load_cached(yaml_path);
        }
    };

    let mut req = client.get(url);
    if let Some(etag) = &cached_etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("network error fetching Ludusavi manifest: {e}; falling back to cache");
            return load_cached(yaml_path);
        }
    };

    match resp.status() {
        reqwest::StatusCode::NOT_MODIFIED => {
            info!("Ludusavi manifest cache is up to date (HTTP 304)");
            load_cached(yaml_path)
        }
        s if s.is_success() => {
            let new_etag = resp
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            let body = resp.text().await.context("reading manifest response body")?;
            fs::write(yaml_path, &body)
                .with_context(|| format!("writing manifest cache `{}`", yaml_path.display()))?;
            if let Some(e) = new_etag {
                let _ = fs::write(etag_path, e);
            }
            info!(bytes = body.len(), "Ludusavi manifest downloaded");
            parse_manifest(&body)
        }
        s => {
            warn!("Ludusavi manifest URL returned status {s}; falling back to cache");
            load_cached(yaml_path)
                .with_context(|| format!("HTTP status {s} and no usable cache"))
        }
    }
}

fn load_cached(yaml_path: &Path) -> Result<Manifest> {
    let yaml = fs::read_to_string(yaml_path)
        .with_context(|| format!("reading cached manifest `{}`", yaml_path.display()))?;
    parse_manifest(&yaml)
}

fn parse_manifest(yaml: &str) -> Result<Manifest> {
    serde_yaml::from_str(yaml).context("parsing Ludusavi manifest YAML")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_entry() {
        let yaml = r#"
"My Game":
  installDir:
    "My Game Folder": {}
  files:
    "<winDocuments>/MyGame/Saves":
      tags:
        - save
      when:
        - os: windows
"#;
        let m: Manifest = parse_manifest(yaml).unwrap();
        let game = m.get("My Game").unwrap();
        assert!(game.install_dir.contains_key("My Game Folder"));
        let f = game.files.get("<winDocuments>/MyGame/Saves").unwrap();
        assert_eq!(f.tags, vec!["save".to_string()]);
        assert_eq!(f.when[0].os.as_deref(), Some("windows"));
    }

    #[test]
    fn ignores_unknown_fields() {
        // Real entries have many fields we don't model — must not fail.
        let yaml = r#"
"Game X":
  cloud:
    steam: true
  launch:
    "<base>/game.exe":
      - when:
          - os: windows
            store: steam
  registry:
    HKEY_CURRENT_USER/SOFTWARE/Foo: {}
  steam:
    id: 12345
  installDir:
    "Game X": {}
"#;
        let m: Manifest = parse_manifest(yaml).unwrap();
        assert!(m.contains_key("Game X"));
    }

    /// Minimal valid manifest used across the HTTP-flow tests below.
    const SAMPLE_MANIFEST: &str = r#"
"My Game":
  installDir:
    "My Game Folder": {}
  files:
    "<winDocuments>/MyGame/Saves":
      tags:
        - save
      when:
        - os: windows
"#;

    #[test]
    fn first_fetch_writes_cache_and_etag() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("GET", "/manifest.yaml")
            .with_status(200)
            .with_header("etag", "\"abc123\"")
            .with_body(SAMPLE_MANIFEST)
            .expect(1)
            .create();

        let cache = tempfile::tempdir().unwrap();
        let url = format!("{}/manifest.yaml", server.url());

        let manifest = ensure_manifest_blocking_with(&url, cache.path(), true).unwrap();
        assert!(manifest.contains_key("My Game"));

        let yaml_cached = fs::read_to_string(cache.path().join("manifest.yaml")).unwrap();
        assert!(yaml_cached.contains("My Game"));
        let etag_cached = fs::read_to_string(cache.path().join("manifest.etag")).unwrap();
        assert_eq!(etag_cached, "\"abc123\"");

        m.assert();
    }

    #[test]
    fn second_fetch_with_matching_etag_uses_cache_on_304() {
        let mut server = mockito::Server::new();

        // First call: 200 + ETag → cache populated.
        let m1 = server
            .mock("GET", "/manifest.yaml")
            .with_status(200)
            .with_header("etag", "\"v1\"")
            .with_body(SAMPLE_MANIFEST)
            .expect(1)
            .create();

        let cache = tempfile::tempdir().unwrap();
        let url = format!("{}/manifest.yaml", server.url());
        ensure_manifest_blocking_with(&url, cache.path(), true).unwrap();
        m1.assert();

        // Second call: server replies 304 only if If-None-Match: "v1".
        let m2 = server
            .mock("GET", "/manifest.yaml")
            .match_header("if-none-match", "\"v1\"")
            .with_status(304)
            .expect(1)
            .create();

        let manifest = ensure_manifest_blocking_with(&url, cache.path(), true).unwrap();
        assert!(manifest.contains_key("My Game"));
        m2.assert();
    }

    #[test]
    fn falls_back_to_cache_when_server_unreachable() {
        // Pre-seed the cache with a valid manifest, then point the URL at a
        // dead address — `ensure_manifest_blocking_with` should not fail.
        let cache = tempfile::tempdir().unwrap();
        fs::write(cache.path().join("manifest.yaml"), SAMPLE_MANIFEST).unwrap();

        // Loopback port 1 is reserved and refuses connections — reqwest fails
        // synchronously, exercising the network-error branch.
        let manifest =
            ensure_manifest_blocking_with("http://127.0.0.1:1/manifest.yaml", cache.path(), true)
                .unwrap();
        assert!(manifest.contains_key("My Game"));
    }

    #[test]
    fn offline_without_cache_returns_error() {
        let cache = tempfile::tempdir().unwrap();
        let err = ensure_manifest_blocking_with("http://unused/", cache.path(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("offline") || msg.contains("cache"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parses_real_manifest_subset() {
        // A realistic sample including macOS paths and an entry with no `files` at all.
        let yaml = r#"
Hades:
  cloud:
    epic: true
    steam: true
  files:
    "<home>/Library/Application Support/Supergiant Games/Hades":
      tags:
        - config
        - save
      when:
        - os: mac
    "<winDocuments>/Saved Games/Hades":
      tags:
        - config
        - save
      when:
        - os: windows
  installDir:
    Hades: {}
  steam:
    id: 1145360

"!4RC4N01D!":
  steam:
    id: 777010
"#;
        let m: Manifest = parse_manifest(yaml).unwrap();
        let hades = m.get("Hades").unwrap();
        assert_eq!(hades.files.len(), 2);
        let arc = m.get("!4RC4N01D!").unwrap();
        assert!(arc.files.is_empty());
        assert!(arc.install_dir.is_empty());
    }
}
