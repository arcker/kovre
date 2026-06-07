mod cli;
mod serve;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, info_span, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use kovre_core::backup::{self, BackupSource, SnapshotInfo};
use kovre_core::config::{Config, Job, Retention};
use kovre_core::templates;

use crate::cli::{Cli, Command, RunArgs};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let cfg = match Config::load(&cli.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };

    let level = cli.log_level.as_deref().unwrap_or(&cfg.agent.log_level);
    if let Err(err) = init_tracing(level) {
        eprintln!("error: {err}");
        return ExitCode::from(2);
    }

    let result = run(cli, cfg);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(level: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .with_context(|| format!("invalid log level `{level}`"))?;

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();
    Ok(())
}

fn run(cli: Cli, cfg: Config) -> Result<()> {
    match cli.command {
        Command::ListJobs => list_jobs(&cfg),
        Command::Run(args) => cmd_run(&cfg, &args),
        Command::ListSnapshots { job } => cmd_list_snapshots(&cfg, &job),
        Command::InitRepo { repository } => cmd_init_repo(&cfg, &repository),
        Command::Serve(args) => serve::run(&cfg, cli.config.clone(), args),
    }
}

fn list_jobs(cfg: &Config) -> Result<()> {
    if cfg.jobs.is_empty() {
        println!("(no jobs configured)");
        return Ok(());
    }

    let mut rows: Vec<[String; 4]> = Vec::with_capacity(cfg.jobs.len() + 1);
    rows.push([
        "NAME".into(),
        "TEMPLATE".into(),
        "REPOSITORY".into(),
        "RETENTION".into(),
    ]);
    for (name, job) in &cfg.jobs {
        rows.push([
            name.clone(),
            job.template.clone().unwrap_or_else(|| "(custom)".into()),
            job.repository.clone(),
            format_retention(job.retention.as_ref()),
        ]);
    }

    let widths = column_widths(&rows);
    for (i, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (col, cell) in row.iter().enumerate() {
            if col > 0 {
                line.push_str("  ");
            }
            if col == row.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}", width = widths[col]));
            }
        }
        println!("{line}");
        if i == 0 {
            // separator under the header
            let total: usize = widths.iter().sum::<usize>() + 2 * (widths.len() - 1);
            println!("{:-<total$}", "");
        }
    }

    Ok(())
}

fn column_widths(rows: &[[String; 4]]) -> [usize; 4] {
    let mut w = [0usize; 4];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            w[i] = w[i].max(cell.chars().count());
        }
    }
    w
}

fn format_retention(retention: Option<&Retention>) -> String {
    let Some(r) = retention else {
        return "-".into();
    };
    let mut parts = Vec::new();
    if let Some(v) = r.keep_last {
        parts.push(format!("last={v}"));
    }
    if let Some(v) = r.keep_hourly {
        parts.push(format!("hourly={v}"));
    }
    if let Some(v) = r.keep_daily {
        parts.push(format!("daily={v}"));
    }
    if let Some(v) = r.keep_weekly {
        parts.push(format!("weekly={v}"));
    }
    if let Some(v) = r.keep_monthly {
        parts.push(format!("monthly={v}"));
    }
    if let Some(v) = r.keep_yearly {
        parts.push(format!("yearly={v}"));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(",")
    }
}

fn cmd_run(cfg: &Config, args: &RunArgs) -> Result<()> {
    if args.all {
        info!(jobs = cfg.jobs.len(), "run --all requested");
        let mut failures = 0usize;
        for name in cfg.jobs.keys() {
            if let Err(err) = run_one(cfg, name) {
                error!(job = name, "{err:#}");
                failures += 1;
            }
        }
        if failures > 0 {
            anyhow::bail!("{failures} job(s) failed");
        }
    } else if let Some(name) = &args.job {
        run_one(cfg, name)?;
    } else {
        // Should be unreachable thanks to clap's ArgGroup, but be defensive.
        anyhow::bail!("either a job name or --all must be specified");
    }
    Ok(())
}

fn run_one(cfg: &Config, name: &str) -> Result<()> {
    let job: &Job = cfg
        .jobs
        .get(name)
        .with_context(|| format!("unknown job `{name}`"))?;
    let repo = cfg
        .repositories
        .get(&job.repository)
        .with_context(|| format!("job `{name}` references unknown repository `{}`", job.repository))?;

    let span = info_span!("job", name = name);
    let _enter = span.enter();

    let resolved = templates::resolve_job(job)
        .with_context(|| format!("resolving job `{name}` source"))?;
    if resolved.paths.is_empty() {
        warn!("job `{name}` has no paths to back up — skipping");
        return Ok(());
    }

    let source = BackupSource {
        paths: resolved.paths,
        excludes: resolved.excludes,
        path_labels: resolved.path_labels,
    };
    let snap = backup::engine_for(repo)
        .backup(name, source, None)
        .with_context(|| format!("running job `{name}`"))?;

    info!(
        snapshot = %snap.id,
        bytes = snap.total_bytes_processed.unwrap_or(0),
        added = snap.data_added.unwrap_or(0),
        "snapshot created"
    );

    if let Some(retention) = &job.retention {
        match backup::engine_for(repo).apply_retention(name, retention) {
            Ok(outcome) => {
                if outcome.forgotten > 0 || outcome.kept > 0 {
                    info!(
                        kept = outcome.kept,
                        forgotten = outcome.forgotten,
                        "retention applied"
                    );
                }
            }
            Err(err) => {
                // Don't fail the job — the new snapshot is already saved. Just log.
                warn!("retention failed for job `{name}`: {err:#}");
            }
        }
    }

    Ok(())
}

fn cmd_list_snapshots(cfg: &Config, job_name: &str) -> Result<()> {
    let job = cfg
        .jobs
        .get(job_name)
        .with_context(|| format!("unknown job `{job_name}`"))?;
    let repo = cfg
        .repositories
        .get(&job.repository)
        .with_context(|| format!("job `{job_name}` references unknown repository `{}`", job.repository))?;

    let snaps = backup::engine_for(repo)
        .list_snapshots(job_name)
        .with_context(|| format!("listing snapshots for job `{job_name}`"))?;

    if snaps.is_empty() {
        println!("(no snapshots tagged for job `{job_name}` in repository `{}`)", job.repository);
        return Ok(());
    }

    print_snapshots(&snaps);
    Ok(())
}

fn print_snapshots(snaps: &[SnapshotInfo]) {
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(snaps.len() + 1);
    rows.push([
        "ID".into(),
        "TIME".into(),
        "HOSTNAME".into(),
        "BYTES".into(),
        "PATHS".into(),
    ]);
    for s in snaps {
        rows.push([
            s.id.chars().take(8).collect(),
            s.time.clone(),
            s.hostname.clone(),
            s.total_bytes_processed
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into()),
            s.paths.join(","),
        ]);
    }

    let mut widths = [0usize; 5];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    for (i, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (col, cell) in row.iter().enumerate() {
            if col > 0 {
                line.push_str("  ");
            }
            if col == row.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}", width = widths[col]));
            }
        }
        println!("{line}");
        if i == 0 {
            let total: usize = widths.iter().sum::<usize>() + 2 * (widths.len() - 1);
            println!("{:-<total$}", "");
        }
    }
}

fn cmd_init_repo(cfg: &Config, repository: &str) -> Result<()> {
    let repo = cfg
        .repositories
        .get(repository)
        .with_context(|| format!("unknown repository `{repository}`"))?;

    info!(repository = repository, path = %repo.path.display(), "initializing repository");
    backup::engine_for(repo)
        .init()
        .with_context(|| format!("initializing repository `{repository}`"))?;
    println!(
        "repository `{repository}` initialized at `{}`",
        repo.path.display()
    );
    Ok(())
}
