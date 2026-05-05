//! Embedded dashboard server (`kovre serve`).
//!
//! Phase 2 step 2: skeleton only. Brings up a Lithair server bound to the
//! configured address, exposes its built-in `/health`, `/ready`, `/info`
//! endpoints, and (with `--debug`) the admin panel at `/_admin/*`. Models,
//! custom routes, and the SvelteKit frontend land in subsequent steps.

use anyhow::{Context, Result};
use kovre_core::config::Config;
use lithair_core::LithairServer;
use tracing::info;

use crate::cli::ServeArgs;

/// Entry point dispatched from `main::run` on `Command::Serve`.
///
/// Builds a multi-threaded Tokio runtime locally rather than wrapping the
/// whole binary in `#[tokio::main]`: the CLI subcommands (`run`, `list-jobs`,
/// …) stay synchronous and pay no runtime startup cost. Only `serve` needs
/// async, and only `serve` builds a runtime.
pub fn run(_cfg: &Config, args: ServeArgs) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating Tokio runtime for `kovre serve`")?;

    rt.block_on(async move {
        info!(
            bind = %args.bind,
            port = args.port,
            debug = args.debug,
            "starting kovre dashboard"
        );

        let mut server = LithairServer::new()
            .with_host(args.bind.to_string())
            .with_port(args.port);

        if args.debug {
            server = server.with_admin_panel(true);
        }

        server
            .serve()
            .await
            .context("Lithair server terminated with an error")
    })
}
