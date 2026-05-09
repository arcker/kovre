//! Build script: ensure `../web/build/` exists so `rust-embed` does not
//! refuse to compile.
//!
//! Reasons it might be missing:
//!   - fresh checkout where `npm run build` has not run yet,
//!   - SvelteKit's adapter-static wipes `web/build/` at the start of each
//!     build, which removes the tracked `.gitkeep` placeholder.
//!
//! Re-creating an empty placeholder is harmless: a real `npm run build`
//! later overwrites the directory with the actual assets, and rust-embed
//! picks them up. Without this script, anyone who has not run the
//! frontend build would fail `cargo build` with a confusing error
//! about a missing folder.

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let build_dir = manifest_dir.join("..").join("web").join("build");
    if !build_dir.exists() {
        fs::create_dir_all(&build_dir).expect("create web/build/");
    }
    let gitkeep = build_dir.join(".gitkeep");
    if !gitkeep.exists() {
        fs::write(
            &gitkeep,
            "# Placeholder so rust-embed can compile when the SvelteKit \
             frontend has not been built yet. Re-created by kovre/build.rs.\n",
        )
        .expect("write web/build/.gitkeep");
    }

    // Re-run only when the build.rs itself changes; the embed snapshot
    // is rebuilt by rust-embed independently.
    println!("cargo:rerun-if-changed=build.rs");
}
