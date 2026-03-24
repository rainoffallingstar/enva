//! Environment run command

use crate::backend::factory::build_backend;
use crate::backend::{BackendKind, BackendSelector, EnvironmentTarget, RunRequest};
use crate::error::{EnvError, Result};
use crate::package_manager::{PackageManager, PackageManagerDetector};
use clap::Args;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

/// Environment run arguments
/// Supports both positional and flag-based syntax:
/// - Positional: enva run <env> <cmd>
/// - Flags: enva run --name <env> --command "<cmd>"
/// - Explicit prefix: enva run --prefix /path/to/env -- <cmd>
#[derive(Debug, Clone, Args)]
pub struct EnvRunArgs {
    /// Environment name (can be positional or via --name/-n)
    #[arg(short, long, value_name = "ENV")]
    pub name: Option<String>,

    /// Explicit package manager to use for environment lookup and execution
    #[arg(long, value_enum)]
    pub pm: Option<PackageManager>,

    /// Explicit environment prefix path; bypasses name-based discovery when provided
    #[arg(long, value_name = "PREFIX")]
    pub prefix: Option<PathBuf>,

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
        if let Some(ref name) = self.name {
            return Ok(name.clone());
        }

        if !self.args.is_empty() && self.prefix.is_none() {
            return Ok(self.args[0].clone());
        }

        Err(EnvError::Validation("Missing environment name".to_string()))
    }

    /// Get command (resolve from either --command flag or positional args)
    pub fn get_command(&self) -> Result<String> {
        if let Some(ref cmd) = self.command {
            return Ok(cmd.clone());
        }

        let start_idx = if self.name.is_some() || self.prefix.is_some() {
            0
        } else {
            1
        };
        let args: Vec<&str> = self
            .args
            .iter()
            .skip(start_idx)
            .map(|value| value.as_str())
            .collect();

        if args.is_empty() {
            return Err(EnvError::Validation("Missing command".to_string()));
        }

        Ok(args.join(" "))
    }
}

#[derive(Clone)]
struct ResolvedEnvironment {
    backend: Arc<dyn crate::backend::EnvironmentBackend>,
    backend_kind: BackendKind,
    package_manager: Option<PackageManager>,
    prefix: PathBuf,
    requested_name: Option<String>,
}

fn format_package_managers(managers: &[PackageManager]) -> String {
    managers
        .iter()
        .map(|pm| pm.to_string())
        .collect::<Vec<String>>()
        .join(", ")
}

fn backend_label(kind: BackendKind, package_manager: Option<PackageManager>) -> String {
    match (kind, package_manager) {
        (BackendKind::Cli, Some(pm)) => pm.to_string(),
        (BackendKind::Cli, None) => "cli".to_string(),
        (BackendKind::Rattler, _) => "rattler".to_string(),
    }
}

fn format_candidates(candidates: &[ResolvedEnvironment]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}",
                backend_label(candidate.backend_kind, candidate.package_manager),
                candidate.prefix.display()
            )
        })
        .collect::<Vec<String>>()
        .join(", ")
}

fn select_package_managers(
    requested_pm: Option<PackageManager>,
    available: &[PackageManager],
) -> Result<Vec<PackageManager>> {
    if available.is_empty() {
        return Err(EnvError::Execution(
            "No package manager found and auto-install failed.".to_string(),
        ));
    }

    if let Some(pm) = requested_pm {
        if available.contains(&pm) {
            return Ok(vec![pm]);
        }

        return Err(EnvError::Execution(format!(
            "Requested package manager '{}' is not available. Available managers: {}",
            pm,
            format_package_managers(available)
        )));
    }

    Ok(available.to_vec())
}

fn validate_backend_request(
    selector: &BackendSelector,
    requested_pm: Option<PackageManager>,
) -> Result<()> {
    if selector.kind == BackendKind::Rattler && requested_pm.is_some() {
        return Err(EnvError::Validation(
            "--pm can only be used with the CLI backend".to_string(),
        ));
    }

    Ok(())
}

fn available_package_managers(requested_pm: Option<PackageManager>) -> Result<Vec<PackageManager>> {
    let detector = PackageManagerDetector::new();
    let available = detector.available_managers_with_env_override();
    select_package_managers(requested_pm, &available)
}

async fn resolve_environment_candidates_for_manager(
    env_name: &str,
    package_manager: PackageManager,
) -> Result<Vec<ResolvedEnvironment>> {
    let backend = build_backend(BackendSelector::cli(Some(package_manager))).await?;
    let prefixes = backend.find_environment_prefixes(env_name).await?;

    Ok(prefixes
        .into_iter()
        .map(|prefix| ResolvedEnvironment {
            backend: backend.clone(),
            backend_kind: BackendKind::Cli,
            package_manager: Some(package_manager),
            prefix,
            requested_name: Some(env_name.to_string()),
        })
        .collect())
}

async fn resolve_environment_by_name(
    env_name: &str,
    selector: BackendSelector,
    requested_pm: Option<PackageManager>,
) -> Result<ResolvedEnvironment> {
    validate_backend_request(&selector, requested_pm)?;

    match selector.kind {
        BackendKind::Cli => {
            let package_managers = available_package_managers(requested_pm)?;
            let mut candidates = Vec::new();

            match package_managers.as_slice() {
                [package_manager] => {
                    candidates.extend(
                        resolve_environment_candidates_for_manager(env_name, *package_manager)
                            .await?,
                    );
                }
                [first, second] => {
                    let (first_result, second_result) = tokio::join!(
                        resolve_environment_candidates_for_manager(env_name, *first),
                        resolve_environment_candidates_for_manager(env_name, *second),
                    );
                    candidates.extend(first_result?);
                    candidates.extend(second_result?);
                }
                [first, second, third] => {
                    let (first_result, second_result, third_result) = tokio::join!(
                        resolve_environment_candidates_for_manager(env_name, *first),
                        resolve_environment_candidates_for_manager(env_name, *second),
                        resolve_environment_candidates_for_manager(env_name, *third),
                    );
                    candidates.extend(first_result?);
                    candidates.extend(second_result?);
                    candidates.extend(third_result?);
                }
                _ => {
                    for package_manager in package_managers.iter().copied() {
                        candidates.extend(
                            resolve_environment_candidates_for_manager(env_name, package_manager)
                                .await?,
                        );
                    }
                }
            }

            if candidates.is_empty() {
                return Err(EnvError::Execution(format!(
                    "Environment '{}' was not found in any available package manager. Searched: {}",
                    env_name,
                    format_package_managers(&package_managers)
                )));
            }

            let selected = candidates[0].clone();
            if candidates.len() > 1 {
                warn!(
                    "Environment '{}' was found in multiple package managers. Using {}:{} (candidates: {})",
                    env_name,
                    backend_label(selected.backend_kind, selected.package_manager),
                    selected.prefix.display(),
                    format_candidates(&candidates)
                );
            }

            Ok(selected)
        }
        BackendKind::Rattler => {
            let backend = build_backend(selector).await?;
            let prefixes = backend.find_environment_prefixes(env_name).await?;

            if prefixes.is_empty() {
                return Err(EnvError::Execution(format!(
                    "Environment '{}' was not found in accessible environment prefixes",
                    env_name
                )));
            }

            let candidates = prefixes
                .into_iter()
                .map(|prefix| ResolvedEnvironment {
                    backend: backend.clone(),
                    backend_kind: BackendKind::Rattler,
                    package_manager: None,
                    prefix,
                    requested_name: Some(env_name.to_string()),
                })
                .collect::<Vec<ResolvedEnvironment>>();

            let selected = candidates[0].clone();
            if candidates.len() > 1 {
                warn!(
                    "Environment '{}' was found in multiple accessible prefixes. Using {}:{} (candidates: {})",
                    env_name,
                    backend_label(selected.backend_kind, selected.package_manager),
                    selected.prefix.display(),
                    format_candidates(&candidates)
                );
            }

            Ok(selected)
        }
    }
}

async fn resolve_environment_target(
    explicit_prefix: &Path,
    selector: BackendSelector,
    requested_pm: Option<PackageManager>,
) -> Result<ResolvedEnvironment> {
    validate_backend_request(&selector, requested_pm)?;

    match selector.kind {
        BackendKind::Cli => {
            let package_manager = available_package_managers(requested_pm)?[0];
            let backend = build_backend(BackendSelector::cli(Some(package_manager))).await?;

            Ok(ResolvedEnvironment {
                backend,
                backend_kind: BackendKind::Cli,
                package_manager: Some(package_manager),
                prefix: explicit_prefix.to_path_buf(),
                requested_name: None,
            })
        }
        BackendKind::Rattler => {
            let backend = build_backend(selector).await?;
            Ok(ResolvedEnvironment {
                backend,
                backend_kind: BackendKind::Rattler,
                package_manager: None,
                prefix: explicit_prefix.to_path_buf(),
                requested_name: None,
            })
        }
    }
}

/// Execute environment run command
pub async fn execute_env_run(args: EnvRunArgs, verbose: bool) -> Result<()> {
    validate_args(&args)?;

    let full_command = build_full_command(&args)?;
    let selector = BackendSelector::from_env();

    let env_name = if args.prefix.is_some() {
        args.name.clone()
    } else {
        Some(args.get_env_name()?)
    };

    if verbose {
        if let Some(ref name) = env_name {
            info!("Executing in environment '{}': {}", name, full_command);
        } else if let Some(ref prefix) = args.prefix {
            info!(
                "Executing in explicit environment prefix '{}': {}",
                prefix.display(),
                full_command
            );
        }
    }

    let resolved = if let Some(ref prefix) = args.prefix {
        resolve_environment_target(prefix, selector.clone(), args.pm).await?
    } else {
        resolve_environment_by_name(
            env_name
                .as_deref()
                .ok_or_else(|| EnvError::Validation("Missing environment name".to_string()))?,
            selector.clone(),
            args.pm,
        )
        .await?
    };

    let ResolvedEnvironment {
        backend,
        backend_kind,
        package_manager,
        prefix,
        requested_name,
    } = resolved;

    let backend_name = backend_label(backend_kind, package_manager);
    if verbose {
        info!(
            "Using backend {} with prefix {}",
            backend_name,
            prefix.display()
        );
        info!("Working directory: {:?}", args.cwd);
        info!("Environment variables: {:?}", args.env);
    } else if requested_name.is_none() {
        info!(
            "Using backend {} with explicit prefix {}",
            backend_name,
            prefix.display()
        );
    }

    match backend
        .run(
            &EnvironmentTarget::Prefix(prefix.clone()),
            &RunRequest {
                command: full_command.clone(),
                env_vars: args.env.clone(),
                cwd: args.cwd.clone(),
                capture_output: !args.no_capture,
            },
        )
        .await
    {
        Ok(_) => {
            if verbose {
                info!("Command executed successfully");
            }
            Ok(())
        }
        Err(error) => {
            let error_msg = format!("{}", error);
            if error_msg.contains("exit code Some(141)") {
                if verbose {
                    info!("Received SIGPIPE (exit code 141), but this is often harmless");
                    info!("Command likely completed successfully before pipe was closed");
                }
                Ok(())
            } else {
                error!("Failed to execute command: {}", error);
                Err(error)
            }
        }
    }
}

/// Build the full command string from arguments
fn build_full_command(args: &EnvRunArgs) -> Result<String> {
    if let Some(ref script) = args.script {
        let mut cmd = format!("Rscript {}", script.display());

        if !args.args.is_empty() {
            cmd.push(' ');
            cmd.push_str(&args.args.join(" "));
        }

        return Ok(cmd);
    }

    args.get_command()
}

/// Validate command arguments
fn validate_args(args: &EnvRunArgs) -> Result<()> {
    let has_positional_cmd = if args.name.is_some() || args.prefix.is_some() {
        !args.args.is_empty()
    } else {
        args.args.len() > 1
    };

    if args.command.is_none() && args.script.is_none() && !has_positional_cmd {
        return Err(EnvError::Validation(
            "Must specify either --command, --script, or positional command".to_string(),
        ));
    }

    if args.command.is_some() && args.script.is_some() {
        return Err(EnvError::Validation(
            "Cannot specify both --command and --script".to_string(),
        ));
    }

    if args.prefix.is_none() && args.name.is_none() && args.args.is_empty() {
        return Err(EnvError::Validation(
            "Must specify an environment name or --prefix".to_string(),
        ));
    }

    if let Some(ref script) = args.script {
        if !script.exists() {
            return Err(EnvError::Validation(format!(
                "Script file does not exist: {}",
                script.display()
            )));
        }
    }

    if let Some(ref prefix) = args.prefix {
        if !prefix.exists() {
            return Err(EnvError::Validation(format!(
                "Environment prefix does not exist: {}",
                prefix.display()
            )));
        }
    }

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
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
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
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
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
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
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
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
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

    #[test]
    fn test_validate_args_with_prefix_only() {
        let args = EnvRunArgs {
            name: None,
            pm: None,
            prefix: Some(PathBuf::from(".")),
            command: Some("echo test".to_string()),
            script: None,
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        let result = validate_args(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_command_with_prefix_uses_all_positional_args() {
        let args = EnvRunArgs {
            name: None,
            pm: None,
            prefix: Some(PathBuf::from(".")),
            command: None,
            script: None,
            args: vec!["echo".to_string(), "hello".to_string()],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        assert_eq!(args.get_command().unwrap(), "echo hello");
    }

    #[test]
    fn test_select_package_managers_honors_explicit_pm() {
        let selected = select_package_managers(
            Some(PackageManager::Conda),
            &[PackageManager::Micromamba, PackageManager::Conda],
        )
        .unwrap();
        assert_eq!(selected, vec![PackageManager::Conda]);
    }

    #[test]
    fn test_select_package_managers_errors_when_pm_missing() {
        let result = select_package_managers(
            Some(PackageManager::Mamba),
            &[PackageManager::Micromamba, PackageManager::Conda],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_build_full_command_from_command_flag() {
        let args = EnvRunArgs {
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
            command: Some("echo test".to_string()),
            script: None,
            args: vec![],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        assert_eq!(build_full_command(&args).unwrap(), "echo test");
    }

    #[test]
    fn test_validate_backend_request_rejects_pm_for_rattler() {
        let result = validate_backend_request(
            &BackendSelector {
                kind: BackendKind::Rattler,
                package_manager: None,
            },
            Some(PackageManager::Micromamba),
        );
        assert!(result.is_err());
    }
}
