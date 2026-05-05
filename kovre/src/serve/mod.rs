//! Embedded dashboard server (`kovre serve`).
//!
//! Brings up a Lithair server bound to the configured address with the
//! built-in `/health`, `/ready`, `/info` endpoints, the dashboard models
//! (currently `JobRun`) under `/api/*`, and (with `--debug`) the admin
//! panel at `/_admin/*`. Custom routes, the kovre.yaml ↔ runtime sync,
//! and the SvelteKit frontend land in subsequent steps.

pub mod models;

use anyhow::{Context, Result};
use kovre_core::config::Config;
use lithair_core::LithairServer;
use tracing::info;

use crate::cli::ServeArgs;
use crate::serve::models::JobRun;

/// Entry point dispatched from `main::run` on `Command::Serve`.
///
/// Builds a multi-threaded Tokio runtime locally rather than wrapping the
/// whole binary in `#[tokio::main]`: the CLI subcommands (`run`, `list-jobs`,
/// …) stay synchronous and pay no runtime startup cost. Only `serve` needs
/// async, and only `serve` builds a runtime.
pub fn run(cfg: &Config, args: ServeArgs) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating Tokio runtime for `kovre serve`")?;

    // `<agent.data_dir>/lithair/` holds Lithair's event-sourced state.
    // Each registered model gets its own subdirectory under this path.
    // Step 5 will surface this as `agent.dashboard.raftlog_dir` so it can
    // be overridden independently of `data_dir`; for now we derive it.
    let lithair_dir = cfg.agent.data_dir.join("lithair");
    let job_runs_path = lithair_dir.join("job_runs");
    let job_runs_path_str = job_runs_path.to_string_lossy().to_string();

    rt.block_on(async move {
        info!(
            bind = %args.bind,
            port = args.port,
            debug = args.debug,
            data_dir = %lithair_dir.display(),
            "starting kovre dashboard"
        );

        let mut server = LithairServer::new()
            .with_host(args.bind.to_string())
            .with_port(args.port)
            .with_model::<JobRun>(job_runs_path_str, "/api/job_runs");

        if args.debug {
            server = server.with_admin_panel(true);
        }

        server
            .serve()
            .await
            .context("Lithair server terminated with an error")
    })
}
