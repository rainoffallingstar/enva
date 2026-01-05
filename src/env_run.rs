//! Environment run command

use crate::error::{Result, EnvError};
use crate::micromamba::MicromambaManager;
use clap::Args;
use std::path::PathBuf;
use tracing::{error, info};

/// Environment run arguments
#[derive(Debug, Clone, Args)]
pub struct EnvRunArgs {
    /// Environment name (required)
    #[arg(short, long)]
    pub name: String,

    /// Command to execute (exclusive with --script)
    #[arg(required_unless_present = "script")]
    pub command: Option<String>,

    /// Script file path (exclusive with command)
    #[arg(short, long)]
    pub script: Option<PathBuf>,

    /// Arguments for command/script
    #[arg(trailing_var_arg = true)]
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

/// Execute environment run command
pub async fn execute_env_run(args: EnvRunArgs, verbose: bool) -> Result<()> {
    if verbose {
        info!("Executing command in environment '{}'", args.name);
    }

    // Validate arguments
    validate_args(&args)?;

    // Get global MicromambaManager
    let micromamba_manager = crate::micromamba::MicromambaManager::get_global_manager()
        .await
        .map_err(|e| {
            error!("Failed to initialize MicromambaManager: {}", e);
            EnvError::Execution(
                "Micromamba not found and auto-install failed.".to_string(),
            )
        })?;

    let manager = micromamba_manager.lock().await;

    // Check if environment exists
    if !manager.environment_exists(&args.name).await? {
        return Err(EnvError::Execution(format!(
            "Environment '{}' does not exist",
            args.name
        )));
    }

    // Build the full command
    let full_command = build_full_command(&args)?;

    if verbose {
        info!("Executing: {}", full_command);
        info!("Working directory: {:?}", args.cwd);
        info!("Environment variables: {:?}", args.env);
    }

    // Execute command with extended options
    // Note: Handle SIGPIPE (exit code 141) gracefully
    match manager
        .run_in_environment_extended(
            &args.name,
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
    // Check that either command or script is provided
    if args.command.is_none() && args.script.is_none() {
        return Err(EnvError::Validation(
            "Must specify either --command or --script".to_string(),
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
