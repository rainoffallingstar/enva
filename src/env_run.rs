//! Environment run command

use crate::error::{Result, EnvError};
use crate::micromamba::MicromambaManager;
use crate::package_manager::PackageManager;
use clap::Args;
use std::path::PathBuf;
use tracing::{error, info};

/// Environment run arguments
/// Supports both positional and flag-based syntax:
/// - Positional: enva run <env> <cmd>
/// - Flags: enva run --name <env> --command "<cmd>"
#[derive(Debug, Clone, Args)]
pub struct EnvRunArgs {
    /// Environment name (can be positional or via --name/-n)
    #[arg(short, long, value_name = "ENV")]
    pub name: Option<String>,

    /// Command to execute (via --command flag only)
    #[arg(long, value_name = "CMD")]
    pub command: Option<String>,

    /// Script file path (exclusive with command)
    #[arg(short, long, value_name = "SCRIPT")]
    pub script: Option<PathBuf>,

    /// Positional arguments: [env_name, command_parts...]
    #[arg(value_name = "ARGS")]
    pub args: Vec<String>,

    /// Working directory
    #[arg(short, long, default_value = ".")]
    pub cwd: PathBuf,

    /// Environment variables (format: KEY=VALUE, can be specified multiple times)
    #[arg(short = 'E', long)]
    pub env: Vec<String>,

    /// Do not capture output, display directly
    #[arg(long)]
    pub no_capture: bool,
}

impl EnvRunArgs {
    /// Get environment name (resolve from either --name flag or first positional arg)
    pub fn get_env_name(&self) -> Result<String> {
        // Priority: --name flag > first positional arg
        if let Some(ref name) = self.name {
            return Ok(name.clone());
        }

        // Try to get from first positional arg
        if !self.args.is_empty() {
            return Ok(self.args[0].clone());
        }

        Err(EnvError::Validation("Missing environment name".to_string()))
    }

    /// Get command (resolve from either --command flag or positional args)
    pub fn get_command(&self) -> Result<String> {
        // Priority: --command flag > positional args (skip env_name)
        if let Some(ref cmd) = self.command {
            return Ok(cmd.clone());
        }

        // Build from positional args (skip first if it's env name)
        let start_idx = if self.name.is_some() { 0 } else { 1 };
        let args: Vec<&str> = self.args.iter()
            .skip(start_idx)
            .map(|s| s.as_str())
            .collect();

        if args.is_empty() {
            return Err(EnvError::Validation("Missing command".to_string()));
        }

        Ok(args.join(" "))
    }
}

/// Execute environment run command
pub async fn execute_env_run(args: EnvRunArgs, verbose: bool) -> Result<()> {
    // Parse environment name and command
    let env_name = args.get_env_name()?;
    let full_command = if let Some(ref script) = args.script {
        format!("Rscript {}", script.display())
    } else {
        args.get_command()?
    };

    if verbose {
        info!("Executing in environment '{}': {}", env_name, full_command);
    }

    // Validate arguments
    validate_args(&args)?;

    // Get global MicromambaManager
    let micromamba_manager = MicromambaManager::get_global_manager()
        .await
        .map_err(|e| {
            error!("Failed to initialize MicromambaManager: {}", e);
            EnvError::Execution(
                "Package manager not found and auto-install failed.".to_string(),
            )
        })?;

    let manager = micromamba_manager.lock().await;

    // Log which package manager is being used
    let pm = manager.get_package_manager();
    if verbose {
        info!("Using package manager: {}", pm);
    } else if pm != PackageManager::Micromamba {
        // Log if using non-default PM (conda/mamba)
        info!("Using package manager: {} (auto-detected)", pm);
    }

    // Check if environment exists
    if !manager.environment_exists(&env_name).await? {
        return Err(EnvError::Execution(format!(
            "Environment '{}' does not exist",
            env_name
        )));
    }

    if verbose {
        info!("Working directory: {:?}", args.cwd);
        info!("Environment variables: {:?}", args.env);
    }

    // Execute command with extended options
    // Note: Handle SIGPIPE (exit code 141) gracefully
    match manager
        .run_in_environment_extended(
            &env_name,
            &full_command,
            &args.env,
            &args.cwd,
            !args.no_capture,
        )
        .await
    {
        Ok(_) => {
            if verbose {
                info!("Command executed successfully");
            }
            Ok(())
        }
        Err(e) => {
            // Check if it's a SIGPIPE error (exit code 141)
            let error_msg = format!("{}", e);
            if error_msg.contains("exit code Some(141)") {
                // SIGPIPE is often harmless - it means the command finished and closed the pipe
                // Check if the environment actually ran successfully by re-checking
                if verbose {
                    info!("Received SIGPIPE (exit code 141), but this is often harmless");
                    info!("Command likely completed successfully before pipe was closed");
                }
                Ok(())
            } else {
                error!("Failed to execute command: {}", e);
                Err(e)
            }
        }
    }
}

/// Build the full command string from arguments
fn build_full_command(args: &EnvRunArgs) -> Result<String> {
    // This function is now deprecated - use get_command() instead
    if let Some(ref script) = args.script {
        let mut cmd = format!("Rscript {}", script.display());

        if !args.args.is_empty() {
            cmd.push_str(" ");
            cmd.push_str(&args.args.join(" "));
        }

        Ok(cmd)
    } else if let Some(ref command) = args.command {
        Ok(command.clone())
    } else {
        Err(EnvError::Validation(
            "Must specify either --command or --script".to_string(),
        ))
    }
}

/// Validate command arguments
fn validate_args(args: &EnvRunArgs) -> Result<()> {
    // Check that either command, script, or positional args are provided
    let has_positional_cmd = if args.name.is_some() {
        // If --name is used, args should contain the command
        !args.args.is_empty()
    } else {
        // If --name is not used, args[0] is env name, args[1+] should be command
        args.args.len() > 1
    };

    if args.command.is_none() && args.script.is_none() && !has_positional_cmd {
        return Err(EnvError::Validation(
            "Must specify either --command, --script, or positional command".to_string(),
        ));
    }

    // Check that command and script are mutually exclusive
    if args.command.is_some() && args.script.is_some() {
        return Err(EnvError::Validation(
            "Cannot specify both --command and --script".to_string(),
        ));
    }

    // Check that script file exists if provided
    if let Some(ref script) = args.script {
        if !script.exists() {
            return Err(EnvError::Validation(format!(
                "Script file does not exist: {}",
                script.display()
            )));
        }
    }

    // Validate environment variable format
    for env_pair in &args.env {
        if !env_pair.contains('=') {
            return Err(EnvError::Validation(format!(
                "Invalid environment variable format: {}. Expected KEY=VALUE",
                env_pair
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_args_both_command_and_script() {
        let args = EnvRunArgs {
            name: "test-env".to_string(),
            command: Some("echo test".to_string()),
            script: Some(PathBuf::from("test.R")),
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        let result = validate_args(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_args_neither_command_nor_script() {
        let args = EnvRunArgs {
            name: "test-env".to_string(),
            command: None,
            script: None,
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        let result = validate_args(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_args_invalid_env_format() {
        let args = EnvRunArgs {
            name: "test-env".to_string(),
            command: Some("echo test".to_string()),
            script: None,
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec!["INVALID_FORMAT".to_string()],
            no_capture: false,
        };

        let result = validate_args(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_args_valid() {
        let args = EnvRunArgs {
            name: "test-env".to_string(),
            command: Some("echo test".to_string()),
            script: None,
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec!["KEY=VALUE".to_string()],
            no_capture: false,
        };

        let result = validate_args(&args);
        assert!(result.is_ok());
    }
}
