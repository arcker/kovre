//! Embedded SvelteKit frontend.
//!
//! `web/build/` is baked into the binary at compile time via
//! `rust-embed`. The release binary is therefore self-contained: a
//! single `kovre.exe` ships with the dashboard HTML/JS/CSS/WASM
//! inside.
//!
//! Build pipeline (release):
//!
//! ```text
//! npm --prefix web ci
//! npm --prefix web run build      # populates web/build/
//! cargo build --release           # rust-embed snapshots web/build/
//! ```
//!
//! If `web/build/` is empty (fresh checkout where the frontend has
//! not been built yet), the binary still compiles, but the embedded
//! asset map is empty — the dashboard pages will 404. That is the
//! correct trade-off: developers iterating on the backend can `cargo
//! build` without touching Node, and CI / release pipelines run
//! `npm run build` first.

use bytes::Bytes;
use http_body_util::Full;
use lithair_core::app::{response, RouteResponse, StatusCode};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../web/build/"]
struct Frontend;

/// Try to read an embedded asset by its path inside `web/build/`.
/// Returns the bytes plus the guessed MIME type. `None` if the path
/// is not in the embed.
pub fn read_asset(asset_path: &str) -> Option<(Bytes, &'static str)> {
    let asset = Frontend::get(asset_path)?;
    let mime = mime_guess::from_path(asset_path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    Some((Bytes::from(asset.data.into_owned()), mime))
}

/// HTTP 200 wrapper around `read_asset`. Returns `None` if the asset
/// is missing — the caller decides whether that should fall back to
/// the SPA shell or 404.
///
/// Built via `hyper::Response::builder()` directly because Lithair's
/// `response::*` helpers don't expose custom-header construction (only
/// Content-Type is settable). We need `Cache-Control` here, hence the
/// detour through hyper. This is the last code path in kovre that
/// forces direct deps on `bytes` + `http-body-util` + `hyper`.
pub fn asset_response(asset_path: &str) -> Option<RouteResponse> {
    let (bytes, mime) = read_asset(asset_path)?;
    Some(
        hyper::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", mime)
            // Hashed `_app/immutable/...` paths are content-addressed:
            // safe to cache aggressively. Other paths (index.html,
            // favicon) get the default 5-minute caching from clients.
            .header(
                "cache-control",
                if asset_path.starts_with("_app/immutable/") {
                    "public, max-age=31536000, immutable"
                } else {
                    "public, max-age=300"
                },
            )
            .body(Full::new(bytes))
            .expect("static headers + valid status never fails"),
    )
}

/// Serve the SPA shell (`index.html`). Used by the not-found handler
/// for any GET that is not an API path or an asset — SvelteKit owns
/// client-side routing from there.
pub fn spa_shell() -> Option<RouteResponse> {
    asset_response("index.html")
}

/// Plain 404 used when an embedded asset really cannot be found
/// (e.g. a malformed `_app/...` path) — distinct from the SPA
/// fallback, which is meant for unknown application routes.
pub fn asset_not_found() -> RouteResponse {
    response::text(StatusCode::NOT_FOUND, "asset not found")
}
