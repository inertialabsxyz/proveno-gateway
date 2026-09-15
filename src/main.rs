use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use proveno_gateway::config;
use proveno_gateway::replay::{replay, status_kind};

#[derive(Parser)]
#[command(name = "proveno-gateway", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the agent-facing MCP server.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Lint a Lua program against a principal's tool description.
    Check {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        principal: String,
        file: PathBuf,
    },
    /// Replay a recorded trace with no network access.
    Replay {
        #[arg(long)]
        config: PathBuf,
        trace_id: String,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Serve { .. } | Command::Check { .. } => {
            eprintln!("not implemented");
            ExitCode::from(2)
        }
        Command::Replay { config, trace_id } => replay_command(&config, &trace_id),
    }
}

/// Exits 0 only when the replay matched. A trace that cannot be loaded,
/// verified or compiled fails the same way as a mismatch.
fn replay_command(config_path: &Path, trace_id: &str) -> ExitCode {
    let report = match config::load(config_path)
        .map_err(anyhow::Error::from)
        .and_then(|config| replay(&config, trace_id))
    {
        Ok(report) => report,
        Err(e) => {
            eprintln!("replay failed: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    if !report.matched {
        println!("replay mismatched: {}", report.trace_id);
        for mismatch in &report.mismatches {
            println!("  {mismatch}");
        }
        return ExitCode::FAILURE;
    }
    println!("replay matched: {}", report.trace_id);
    println!("output: {}", report.output.as_deref().unwrap_or("none"));
    println!("status: {}", status_kind(&report.status));
    println!("gas_used: {}", report.gas_used);
    println!("memory_used: {}", report.memory_used);
    ExitCode::SUCCESS
}
