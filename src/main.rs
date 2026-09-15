use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
        Command::Serve { .. } | Command::Check { .. } | Command::Replay { .. } => {
            eprintln!("not implemented");
            ExitCode::from(2)
        }
    }
}
