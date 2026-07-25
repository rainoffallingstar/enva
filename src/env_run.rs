//! Environment run command

use crate::backend::factory::build_backend;
use crate::backend::{
    BackendCapability, BackendKind, BackendSelector, EnvironmentName, EnvironmentResolution,
    EnvironmentTarget, RunCommand, RunRequest,
};
use crate::error::{EnvError, Result};
use crate::package_manager::{PackageManager, PackageManagerDetector};
use clap::Args;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info};

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

    /// Explicit compatibility package manager for CLI fallback mode
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
    pub args: Vec<OsString>,

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
    pub fn get_env_name(&self) -> Result<EnvironmentName> {
        if let Some(name) = &self.name {
            return EnvironmentName::parse(name.clone());
        }

        if self.prefix.is_none() {
            let positional_name = self
                .args
                .first()
                .ok_or_else(|| EnvError::Validation("Missing environment name".to_string()))?;
            let positional_name = positional_name.to_str().ok_or_else(|| {
                EnvError::Validation("Environment name must be valid UTF-8".to_string())
            })?;
            return EnvironmentName::parse(positional_name.to_string());
        }

        Err(EnvError::Validation("Missing environment name".to_string()))
    }

    fn command_arguments(&self) -> &[OsString] {
        let start_index = if self.name.is_some() || self.prefix.is_some() {
            0
        } else {
            1
        };
        &self.args[start_index.min(self.args.len())..]
    }

    pub fn get_run_command(&self) -> Result<RunCommand> {
        if let Some(command) = &self.command {
            return RunCommand::shell(command.clone());
        }

        if let Some(script) = &self.script {
            let mut arguments = Vec::with_capacity(self.command_arguments().len() + 2);
            arguments.push(OsString::from("Rscript"));
            arguments.push(script.as_os_str().to_os_string());
            arguments.extend(self.command_arguments().iter().cloned());
            return RunCommand::argv(arguments);
        }

        RunCommand::argv(self.command_arguments().to_vec())
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

fn require_unique_environment(
    env_name: &str,
    candidates: Vec<ResolvedEnvironment>,
    not_found_message: String,
) -> Result<ResolvedEnvironment> {
    match EnvironmentResolution::from_candidates(candidates) {
        EnvironmentResolution::NotFound => Err(EnvError::Execution(not_found_message)),
        EnvironmentResolution::Unique(environment) => Ok(environment),
        EnvironmentResolution::Ambiguous(environments) => Err(EnvError::Execution(format!(
            "Environment '{}' matched multiple accessible prefixes: {}. Use --prefix to disambiguate.",
            env_name,
            format_candidates(&environments)
        ))),
    }
}

fn select_package_managers(
    requested_pm: Option<PackageManager>,
    available: &[PackageManager],
) -> Result<Vec<PackageManager>> {
    if available.is_empty() {
        return Err(EnvError::Execution(
            "No compatibility package manager is installed. Add conda, mamba, or micromamba to PATH; micromamba may also be configured with ENVA_MICROMAMBA_PATH."
                .to_string(),
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
            "--pm is only available in compatibility mode (ENVA_BACKEND=cli)".to_string(),
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

            require_unique_environment(
                env_name,
                candidates,
                format!(
                    "Environment '{}' was not found in any available package manager. Searched: {}",
                    env_name,
                    format_package_managers(&package_managers)
                ),
            )
        }
        BackendKind::Rattler => {
            let backend = build_backend(selector).await?;
            let candidates = backend
                .find_environment_prefixes(env_name)
                .await?
                .into_iter()
                .map(|prefix| ResolvedEnvironment {
                    backend: backend.clone(),
                    backend_kind: BackendKind::Rattler,
                    package_manager: None,
                    prefix,
                    requested_name: Some(env_name.to_string()),
                })
                .collect::<Vec<ResolvedEnvironment>>();

            require_unique_environment(
                env_name,
                candidates,
                format!(
                    "Environment '{}' was not found in accessible environment prefixes",
                    env_name
                ),
            )
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

#[derive(Debug, Clone)]
pub(crate) struct ResolvedEnvironmentReference {
    pub prefix: PathBuf,
}

pub(crate) async fn resolve_environment_reference(
    env_name: Option<&str>,
    prefix: Option<&Path>,
    requested_pm: Option<PackageManager>,
) -> Result<ResolvedEnvironmentReference> {
    let selector = BackendSelector::from_env();
    let resolved = if let Some(explicit_prefix) = prefix {
        resolve_environment_target(explicit_prefix, selector, requested_pm).await?
    } else {
        resolve_environment_by_name(
            env_name.ok_or_else(|| EnvError::Validation("Missing environment name".to_string()))?,
            selector,
            requested_pm,
        )
        .await?
    };

    Ok(ResolvedEnvironmentReference {
        prefix: resolved.prefix,
    })
}

/// Execute environment run command
pub async fn execute_env_run(args: EnvRunArgs, verbose: bool) -> Result<()> {
    validate_args(&args)?;

    let run_command = args.get_run_command()?;
    let command_display = run_command.display_lossy();
    let selector = BackendSelector::from_env();

    let env_name = if args.prefix.is_some() {
        args.name
            .as_ref()
            .map(|name| EnvironmentName::parse(name.clone()))
            .transpose()?
    } else {
        Some(args.get_env_name()?)
    };

    if verbose {
        if let Some(name) = &env_name {
            info!("Executing in environment '{}': {}", name, command_display);
        } else if let Some(prefix) = &args.prefix {
            info!(
                "Executing in explicit environment prefix '{}': {}",
                prefix.display(),
                command_display
            );
        }
    }

    let resolved = if let Some(ref prefix) = args.prefix {
        resolve_environment_target(prefix, selector.clone(), args.pm).await?
    } else {
        resolve_environment_by_name(
            env_name
                .as_ref()
                .ok_or_else(|| EnvError::Validation("Missing environment name".to_string()))?
                .as_str(),
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

    backend.require_capability(BackendCapability::RunByPrefix)?;
    match backend
        .run(
            &EnvironmentTarget::Prefix(prefix.clone()),
            &RunRequest {
                command: run_command.clone(),
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
            error!("Failed to execute command: {}", error);
            Err(error)
        }
    }
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
    fn require_unique_environment_rejects_ambiguous_prefixes() {
        let backend: Arc<dyn crate::backend::EnvironmentBackend> = Arc::new(
            crate::backend::cli::CliBackend::new(Some(PackageManager::Conda)),
        );
        let candidates = vec![
            ResolvedEnvironment {
                backend: backend.clone(),
                backend_kind: BackendKind::Cli,
                package_manager: Some(PackageManager::Conda),
                prefix: PathBuf::from("/first/envs/demo"),
                requested_name: Some("demo".to_string()),
            },
            ResolvedEnvironment {
                backend,
                backend_kind: BackendKind::Cli,
                package_manager: Some(PackageManager::Conda),
                prefix: PathBuf::from("/second/envs/demo"),
                requested_name: Some("demo".to_string()),
            },
        ];

        let error = require_unique_environment("demo", candidates, "not found".to_string())
            .err()
            .expect("ambiguous names must fail closed");
        let message = error.to_string();

        assert!(message.contains("matched multiple accessible prefixes"));
        assert!(message.contains("/first/envs/demo"));
        assert!(message.contains("/second/envs/demo"));
        assert!(message.contains("Use --prefix to disambiguate"));
    }

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
    fn test_get_run_command_with_prefix_preserves_all_positional_args() {
        let expected_arguments = vec![
            OsString::from("echo"),
            OsString::from("value with spaces"),
            OsString::from("semi;colon"),
            OsString::from("$(printf injected)"),
            OsString::from("*.fastq.gz"),
            OsString::from("line one\nline two"),
            OsString::from("-leading-option"),
        ];
        let args = EnvRunArgs {
            name: None,
            pm: None,
            prefix: Some(PathBuf::from(".")),
            command: None,
            script: None,
            args: expected_arguments.clone(),
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        assert_eq!(
            args.get_run_command().unwrap(),
            RunCommand::Argv(expected_arguments)
        );
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
    fn test_command_flag_uses_explicit_shell_mode() {
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

        assert_eq!(
            args.get_run_command().unwrap(),
            RunCommand::Shell("echo test".to_string())
        );
    }

    #[test]
    fn test_script_uses_argv_mode_and_preserves_arguments() {
        let script_path = PathBuf::from("script path/test.R");
        let args = EnvRunArgs {
            name: Some("test-env".to_string()),
            pm: None,
            prefix: None,
            command: None,
            script: Some(script_path.clone()),
            args: vec![
                OsString::from("value with spaces"),
                OsString::from("semi;colon"),
                OsString::from("$(printf injected)"),
                OsString::from("*.fastq.gz"),
                OsString::from("line one\nline two"),
                OsString::from("-leading-option"),
            ],
            cwd: PathBuf::from("."),
            env: vec![],
            no_capture: false,
        };

        assert_eq!(
            args.get_run_command().unwrap(),
            RunCommand::Argv(vec![
                OsString::from("Rscript"),
                script_path.into_os_string(),
                OsString::from("value with spaces"),
                OsString::from("semi;colon"),
                OsString::from("$(printf injected)"),
                OsString::from("*.fastq.gz"),
                OsString::from("line one\nline two"),
                OsString::from("-leading-option"),
            ])
        );
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
        let error = result.expect_err("rattler backend should reject --pm");
        assert!(error
            .to_string()
            .contains("compatibility mode (ENVA_BACKEND=cli)"));
    }
}
