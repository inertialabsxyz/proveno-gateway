use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use proveno_gateway::config;
use proveno_gateway::engine::Engine;
use proveno_gateway::replay::{replay, status_kind};
use proveno_gateway::server;

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
        Command::Serve { config } => run(serve(config)),
        Command::Check {
            config,
            principal,
            file,
        } => run(check(config, principal, file)),
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

/// Runs a subcommand on a multi-threaded runtime. A setup failure (config,
/// secrets, downstream connection) exits 2.
fn run(command: impl Future<Output = anyhow::Result<ExitCode>>) -> ExitCode {
    let result = tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(command));
    result.unwrap_or_else(|e| {
        eprintln!("error: {e:#}");
        ExitCode::from(2)
    })
}

async fn serve(config: PathBuf) -> anyhow::Result<ExitCode> {
    server::serve(config::load(&config)?).await?;
    Ok(ExitCode::SUCCESS)
}

/// Prints `ok` and exits 0, or prints `line N: message` and exits 1.
async fn check(config: PathBuf, principal: String, file: PathBuf) -> anyhow::Result<ExitCode> {
    let program =
        std::fs::read_to_string(&file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    let engine = Engine::new(config::load(&config)?).await?;
    match engine.check(&principal, &program) {
        Ok(()) => {
            println!("ok");
            Ok(ExitCode::SUCCESS)
        }
        Err(lint) => {
            println!("{lint}");
            Ok(ExitCode::FAILURE)
        }
    }
}
