//! enva - A rattler-first environment manager for bioinformatics workflows

use clap::Parser;
use enva::env::{execute_env_command, EnvCommand};
use std::io::{self, IsTerminal};
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

fn command_supports_startup_banner(command: &EnvCommand) -> bool {
    !matches!(
        command,
        EnvCommand::Run(_)
            | EnvCommand::Activate(_)
            | EnvCommand::Deactivate(_)
            | EnvCommand::Shell(_)
    )
}

fn should_display_startup_banner(cli: &Cli, standard_error_is_terminal: bool) -> bool {
    !cli.quiet
        && !cli.json
        && standard_error_is_terminal
        && command_supports_startup_banner(&cli.command)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    if should_display_startup_banner(&cli, io::stderr().is_terminal()) {
        enva::display_startup_banner();
    }

    if cli.verbose {
        let _ = tracing_subscriber::fmt::try_init();
    }

    execute_env_command(cli.command, cli.verbose, cli.log, cli.dry_run, cli.json).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{should_display_startup_banner, Cli};
    use clap::Parser;

    fn parse_cli(arguments: &[&str]) -> Cli {
        Cli::try_parse_from(arguments).expect("CLI arguments should parse")
    }

    #[test]
    fn interactive_human_command_displays_banner() {
        let cli = parse_cli(&["enva", "list"]);
        assert!(should_display_startup_banner(&cli, true));
    }

    #[test]
    fn run_command_never_displays_banner() {
        let cli = parse_cli(&["enva", "run", "example", "--", "printf", "ready"]);
        assert!(!should_display_startup_banner(&cli, true));
    }

    #[test]
    fn redirected_output_does_not_display_banner() {
        let cli = parse_cli(&["enva", "list"]);
        assert!(!should_display_startup_banner(&cli, false));
    }

    #[test]
    fn json_and_quiet_modes_do_not_display_banner() {
        let json_cli = parse_cli(&["enva", "--json", "list"]);
        assert!(!should_display_startup_banner(&json_cli, true));

        let quiet_cli = parse_cli(&["enva", "--quiet", "list"]);
        assert!(!should_display_startup_banner(&quiet_cli, true));
    }
}
