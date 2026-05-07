use std::net::IpAddr;
use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "kovre",
    version,
    about = "Backup orchestrator for Windows — declarative YAML, rustic_core engine"
)]
pub struct Cli {
    /// Path to the configuration file
    #[arg(short, long, global = true, default_value = "kovre.yaml")]
    pub config: PathBuf,

    /// Override the log level from the config (trace, debug, info, warn, error)
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run one or all configured backup jobs
    Run(RunArgs),

    /// List all configured jobs
    ListJobs,

    /// List snapshots stored in the repository for a given job
    ListSnapshots {
        /// Name of the job whose snapshots should be listed
        job: String,
    },

    /// Initialize a backup repository (must be done once before the first backup)
    InitRepo {
        /// Name of the repository, as defined in the configuration
        repository: String,
    },

    /// Start the embedded dashboard web server (Phase 2)
    Serve(ServeArgs),
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// TCP port to bind on
    #[arg(long, default_value_t = 18080)]
    pub port: u16,

    /// Address to bind to. `127.0.0.1` (default) restricts the dashboard to
    /// the local machine; use `0.0.0.0` to expose it on the LAN (requires
    /// `agent.dashboard.token_file` in kovre.yaml — enforced in a later step).
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: IpAddr,

    /// Enable Lithair's admin panel at `/_admin/*`. Off by default;
    /// useful for inspecting the event log and raw models during dev.
    #[arg(long)]
    pub debug: bool,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .args(["job", "all"]),
))]
pub struct RunArgs {
    /// Name of the job to run
    pub job: Option<String>,

    /// Run every job declared in the configuration
    #[arg(long)]
    pub all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_list_jobs() {
        let cli = Cli::try_parse_from(["kovre", "list-jobs"]).unwrap();
        assert!(matches!(cli.command, Command::ListJobs));
        assert_eq!(cli.config, PathBuf::from("kovre.yaml"));
    }

    #[test]
    fn parses_run_with_job_name() {
        let cli = Cli::try_parse_from(["kovre", "run", "documents"]).unwrap();
        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.job.as_deref(), Some("documents"));
                assert!(!args.all);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parses_run_all() {
        let cli = Cli::try_parse_from(["kovre", "run", "--all"]).unwrap();
        match cli.command {
            Command::Run(args) => {
                assert!(args.all);
                assert!(args.job.is_none());
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_requires_target() {
        let err = Cli::try_parse_from(["kovre", "run"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn run_rejects_job_with_all() {
        let err = Cli::try_parse_from(["kovre", "run", "documents", "--all"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn config_flag_is_global() {
        let cli =
            Cli::try_parse_from(["kovre", "list-jobs", "--config", "alt.yaml"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("alt.yaml"));
    }

    #[test]
    fn parses_serve_with_defaults() {
        let cli = Cli::try_parse_from(["kovre", "serve"]).unwrap();
        match cli.command {
            Command::Serve(args) => {
                assert_eq!(args.port, 18080);
                assert_eq!(args.bind.to_string(), "127.0.0.1");
                assert!(!args.debug);
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn parses_serve_with_overrides() {
        let cli = Cli::try_parse_from([
            "kovre", "serve", "--port", "9090", "--bind", "0.0.0.0", "--debug",
        ])
        .unwrap();
        match cli.command {
            Command::Serve(args) => {
                assert_eq!(args.port, 9090);
                assert_eq!(args.bind.to_string(), "0.0.0.0");
                assert!(args.debug);
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn serve_rejects_invalid_bind() {
        let err =
            Cli::try_parse_from(["kovre", "serve", "--bind", "not-an-ip"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
}
