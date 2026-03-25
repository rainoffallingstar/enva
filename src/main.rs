//! enva - A rattler-first environment manager for bioinformatics workflows

use clap::Parser;
use enva::env::{execute_env_command, EnvCommand};
use std::path::PathBuf;

/// CLI arguments for enva
#[derive(Debug, Parser)]
#[command(name = "enva")]
#[command(about = "A rattler-first environment manager for bioinformatics workflows")]
#[command(version)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Quiet mode (suppress output)
    #[arg(short, long)]
    quiet: bool,

    /// Log file path
    #[arg(short, long)]
    log: Option<PathBuf>,

    /// Enable dry-run mode (validate without creating)
    #[arg(long)]
    dry_run: bool,

    /// Output in JSON format
    #[arg(long)]
    json: bool,

    /// Environment subcommands
    #[command(subcommand)]
    command: EnvCommand,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Display startup banner (unless in quiet mode)
    if !cli.quiet {
        enva::display_startup_banner();
    }

    // Initialize logging
    if cli.verbose {
        let _ = tracing_subscriber::fmt::try_init();
    }

    // Execute the command
    execute_env_command(cli.command, cli.verbose, cli.log, cli.dry_run, cli.json).await?;

    Ok(())
}
