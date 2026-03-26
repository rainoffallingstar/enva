//! Environment management commands

use crate::backend::factory::build_default_backend;
use crate::backend::OutputMode;
use crate::error::{EnvError, Result};
use crate::micromamba::CondaEnvironment;
use crate::package_manager::PackageManager;
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

/// Environment management arguments
#[derive(Debug, Clone, Args)]
pub struct EnvArgs {
    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Configuration file path
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Enable dry-run mode (validate without creating)
    #[arg(long)]
    pub dry_run: bool,

    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Environment creation arguments
#[derive(Debug, Clone, Args)]
pub struct EnvCreateArgs {
    /// Create all environments
    #[arg(long)]
    pub all: bool,

    /// Create xdxtools-core environment (bioinformatics tools)
    #[arg(long)]
    pub core: bool,

    /// Create xdxtools-snakemake environment (workflow engine)
    #[arg(long)]
    pub snakemake: bool,

    /// Create xdxtools-extra environment (additional tools)
    #[arg(long)]
    pub extra: bool,

    /// Custom YAML configuration file (optional)
    #[arg(short, long)]
    pub yaml: Option<PathBuf>,

    /// Environment name (for custom environments)
    #[arg(long)]
    pub name: Option<String>,

    /// Replace an existing environment before recreating it
    #[arg(long)]
    pub force: bool,

    /// Clean conda package caches before creating environments
    #[arg(long)]
    pub clean_cache: bool,

    /// Additional packages to install immediately after creation (comma-separated or repeated)
    #[arg(long = "with", value_name = "PKG")]
    pub with: Vec<String>,

    /// Terminal output mode: stream full logs, show a concise summary, or stay quiet
    #[arg(long, value_enum, default_value_t = OutputMode::Summary)]
    pub output: OutputMode,
}

/// Environment validation arguments
#[derive(Debug, Clone, Args)]
pub struct EnvValidateArgs {
    /// Validate all environments
    #[arg(long)]
    pub all: bool,

    /// Environment name to validate
    #[arg(long)]
    pub name: Option<String>,
}

/// Environment list arguments
#[derive(Debug, Clone, Args)]
pub struct EnvListArgs {
    /// Show detailed information
    #[arg(long)]
    pub detailed: bool,
}

/// Environment install arguments
#[derive(Debug, Clone, Args)]
pub struct EnvInstallArgs {
    /// Package names to install (comma-separated or multiple flags)
    #[arg(required = true)]
    pub packages: Vec<String>,

    /// Environment name
    #[arg(long)]
    pub name: Option<String>,
}

/// Environment adoption arguments
#[derive(Debug, Clone, Args)]
pub struct EnvAdoptArgs {
    /// Environment name to adopt into rattler ownership
    #[arg(long, conflicts_with = "prefix")]
    pub name: Option<String>,

    /// Explicit prefix path to adopt into rattler ownership
    #[arg(long, value_name = "PREFIX", conflicts_with = "name")]
    pub prefix: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ActivationShell {
    Auto,
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl ActivationShell {
    fn resolved(self) -> Self {
        match self {
            Self::Auto => {
                let shell = std::env::var("SHELL")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if shell.contains("zsh") {
                    Self::Zsh
                } else if shell.contains("fish") {
                    Self::Fish
                } else if shell.contains("pwsh") || shell.contains("powershell") {
                    Self::Powershell
                } else {
                    Self::Bash
                }
            }
            shell => shell,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct EnvActivateArgs {
    /// Environment name
    #[arg(short, long, value_name = "ENV", conflicts_with_all = ["prefix", "env"])]
    pub name: Option<String>,

    /// Explicit compatibility package manager for CLI fallback mode
    #[arg(long, value_enum)]
    pub pm: Option<PackageManager>,

    /// Explicit environment prefix path
    #[arg(long, value_name = "PREFIX", conflicts_with_all = ["name", "env"])]
    pub prefix: Option<PathBuf>,

    /// Shell type used to render activation code
    #[arg(long, value_enum, default_value_t = ActivationShell::Auto)]
    pub shell: ActivationShell,

    /// Positional environment name
    #[arg(value_name = "ENV", conflicts_with_all = ["name", "prefix"])]
    pub env: Option<String>,
}

impl EnvActivateArgs {
    fn requested_env_name(&self) -> Option<String> {
        self.name.clone().or_else(|| self.env.clone())
    }
}

#[derive(Debug, Clone, Args)]
pub struct EnvDeactivateArgs {
    /// Shell type used to render deactivation code
    #[arg(long, value_enum, default_value_t = ActivationShell::Auto)]
    pub shell: ActivationShell,
}

#[derive(Debug, Clone, Args)]
pub struct EnvShellArgs {
    #[command(subcommand)]
    pub command: EnvShellCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum EnvShellCommand {
    /// Emit shell integration code for your shell profile
    Hook(EnvShellHookArgs),
}

#[derive(Debug, Clone, Args)]
pub struct EnvShellHookArgs {
    /// Shell type used to render hook code
    #[arg(value_name = "SHELL", value_enum, default_value_t = ActivationShell::Auto)]
    pub shell: ActivationShell,
}

/// Environment command subcommands
#[derive(Subcommand, Debug)]
pub enum EnvCommand {
    /// Create conda environments
    Create(EnvCreateArgs),

    /// List conda environments
    List(EnvListArgs),

    /// Validate environment configuration
    Validate(EnvValidateArgs),

    /// Install components in environment
    Install(EnvInstallArgs),

    /// Adopt an existing environment into rattler ownership
    Adopt(EnvAdoptArgs),

    /// Remove conda environment
    Remove {
        /// Environment name
        name: String,
    },

    /// Emit shell code to activate an environment in the current shell
    Activate(EnvActivateArgs),

    /// Emit shell code to deactivate the current enva-managed shell state
    Deactivate(EnvDeactivateArgs),

    /// Shell integration helpers
    Shell(EnvShellArgs),

    /// Run command or script in environment
    Run(crate::env_run::EnvRunArgs),
}

/// Execute environment command
pub async fn execute_env_command(
    command: EnvCommand,
    verbose: bool,
    _config: Option<PathBuf>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    match command {
        EnvCommand::Create(args) => execute_env_create(args, verbose, dry_run, json).await,
        EnvCommand::List(args) => execute_env_list(args, verbose, json).await,
        EnvCommand::Validate(args) => execute_env_validate(args, verbose, dry_run, json).await,
        EnvCommand::Install(args) => execute_env_install(args, verbose).await,
        EnvCommand::Adopt(args) => execute_env_adopt(args, verbose).await,
        EnvCommand::Remove { name } => execute_env_remove(name, verbose).await,
        EnvCommand::Activate(args) => execute_env_activate(args, verbose).await,
        EnvCommand::Deactivate(args) => execute_env_deactivate(args, verbose).await,
        EnvCommand::Shell(args) => execute_env_shell(args, verbose).await,
        EnvCommand::Run(args) => crate::env_run::execute_env_run(args, verbose).await,
    }
}

fn execution_output_mode(verbose: bool) -> OutputMode {
    if verbose {
        OutputMode::Stream
    } else {
        OutputMode::Summary
    }
}

fn parse_package_specs(package_specs: &[String]) -> Vec<String> {
    let mut packages = Vec::new();
    for pkg_list in package_specs {
        for pkg in pkg_list
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            packages.push(pkg.to_string());
        }
    }
    packages
}

fn resolve_yaml_file(env_name: &str, yaml_override: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(yaml_path) = yaml_override {
        return Ok(yaml_path.clone());
    }

    let current_dir = std::env::current_dir().map_err(|e| {
        error!("Failed to get current directory: {}", e);
        EnvError::FileOperation(format!("Failed to get current directory: {}", e))
    })?;

    let src_config = current_dir
        .join("src")
        .join("configs")
        .join(format!("{}.yaml", env_name));
    let envs_config = current_dir
        .join("environments")
        .join("configs")
        .join(format!("{}.yaml", env_name));

    if src_config.exists() {
        Ok(src_config)
    } else if envs_config.exists() {
        Ok(envs_config)
    } else {
        Ok(src_config)
    }
}

/// Execute environment creation
async fn execute_env_create(
    args: EnvCreateArgs,
    verbose: bool,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        info!("Starting conda environment creation (dry-run mode)...");
    } else {
        info!("Starting conda environment creation...");
    }

    let backend = build_default_backend().await?;
    let packages_to_install = parse_package_specs(&args.with);

    let mut environments_to_create = Vec::new();

    if args.yaml.is_some() {
        if let Some(ref name) = args.name {
            environments_to_create.push(name.as_str());
        } else {
            return Err(EnvError::Validation(
                "When using --yaml, you must also specify --name for the environment".to_string(),
            ));
        }
    } else {
        if args.all {
            environments_to_create.extend_from_slice(&[
                "xdxtools-core",
                "xdxtools-snakemake",
                "xdxtools-extra",
            ]);
        } else {
            if args.core {
                environments_to_create.push("xdxtools-core");
            }
            if args.snakemake {
                environments_to_create.push("xdxtools-snakemake");
            }
            if args.extra {
                environments_to_create.push("xdxtools-extra");
            }
            if let Some(ref name) = args.name {
                environments_to_create.push(name.as_str());
            }
        }

        if environments_to_create.is_empty() {
            return Err(EnvError::Validation(
                "Must specify either --all, --core, --snakemake, --extra, or --name".to_string(),
            ));
        }
    }

    if verbose {
        info!(
            "Creating {} environments: {:?}",
            environments_to_create.len(),
            environments_to_create
        );
        if !packages_to_install.is_empty() {
            info!(
                "Additional packages to install after creation: {:?}",
                packages_to_install
            );
        }
    }

    if dry_run {
        use serde_json::{json, Value};
        let mut results = Vec::new();

        for env_name in &environments_to_create {
            let yaml_file = resolve_yaml_file(env_name, args.yaml.as_ref())?;
            let file_exists = yaml_file.exists();
            let file_path_str = yaml_file.to_string_lossy().to_string();

            if json {
                results.push(json!({
                    "environment": env_name,
                    "yaml_file": file_path_str,
                    "file_exists": file_exists,
                    "action": "create",
                    "dry_run": true,
                    "additional_packages": packages_to_install.clone(),
                    "status": if file_exists { "ready" } else { "file_not_found" }
                }));
            } else {
                println!("[DRY-RUN] Environment: {}", env_name);
                println!("[DRY-RUN] YAML file: {}", file_path_str);
                println!(
                    "[DRY-RUN] File exists: {}",
                    if file_exists { "YES" } else { "NO" }
                );
                if !packages_to_install.is_empty() {
                    println!(
                        "[DRY-RUN] Additional packages: {}",
                        packages_to_install.join(", ")
                    );
                }
                println!(
                    "[DRY-RUN] Status: {}",
                    if file_exists {
                        "Ready to create"
                    } else {
                        "File not found!"
                    }
                );
                println!("{}", "-".repeat(50));
            }
        }

        if json {
            println!("{}", serde_json::to_string_pretty(&Value::Array(results))?);
        }
        return Ok(());
    }

    let mut success_count = 0;
    let mut failed_count = 0;
    let mut failure_details = Vec::new();

    if args.clean_cache {
        backend.clean_package_cache(dry_run, args.output).await?;
    }

    for env_name in environments_to_create {
        let yaml_file = resolve_yaml_file(env_name, args.yaml.as_ref())?;

        match backend
            .create_environment(env_name, &yaml_file, dry_run, args.force, args.output)
            .await
        {
            Ok(_) => {
                if packages_to_install.is_empty() {
                    success_count += 1;
                    info!("Successfully created environment: {}", env_name);
                } else {
                    match backend
                        .install_packages(env_name, &packages_to_install, args.output)
                        .await
                    {
                        Ok(_) => {
                            success_count += 1;
                            info!(
                                "Successfully created environment {} and installed additional packages",
                                env_name
                            );
                        }
                        Err(e) => {
                            failed_count += 1;
                            let detail = format!(
                                "{}: created environment but failed to install additional packages: {}",
                                env_name, e
                            );
                            error!(
                                "Failed to install additional packages in environment {}: {}",
                                env_name, e
                            );
                            eprintln!(
                                "Failed to install additional packages in environment {}: {}",
                                env_name, e
                            );
                            failure_details.push(detail);
                        }
                    }
                }
            }
            Err(e) => {
                failed_count += 1;
                let detail = format!("{}: {}", env_name, e);
                error!("Failed to create environment {}: {}", env_name, e);
                eprintln!("Failed to create environment {}: {}", env_name, e);
                failure_details.push(detail);
            }
        }
    }

    info!(
        "Environment creation complete: {} succeeded, {} failed",
        success_count, failed_count
    );

    if failed_count > 0 {
        let summary = format!("{} environments failed to create", failed_count);
        if failure_details.is_empty() {
            return Err(EnvError::Execution(summary));
        }

        return Err(EnvError::Execution(format!(
            "{}: {}",
            summary,
            failure_details.join("; ")
        )));
    }

    Ok(())
}

/// Execute environment list
async fn execute_env_list(args: EnvListArgs, _verbose: bool, json: bool) -> Result<()> {
    info!("Listing conda environments...");

    // 直接显示所有 conda 环境
    list_all_conda_environments(args.detailed, json).await
}

/// Execute environment validation
async fn execute_env_validate(
    args: EnvValidateArgs,
    verbose: bool,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        info!("Validating conda environment configuration (dry-run mode)...");
    } else {
        info!("Validating conda environment configuration...");
    }

    let backend = build_default_backend().await?;

    if args.all || args.name.is_none() {
        // Validate all environments by checking if they exist
        let env_names = vec!["xdxtools-core", "xdxtools-snakemake", "xdxtools-extra"];

        if json {
            use serde_json::{json, Value};
            let mut results = Vec::new();

            for env_name in env_names {
                let exists = backend.environment_exists(env_name).await.unwrap_or(false);
                results.push(json!({
                    "environment": env_name,
                    "exists": exists,
                    "valid": exists,
                    "dry_run": dry_run
                }));
            }

            println!("{}", serde_json::to_string_pretty(&Value::Array(results))?);
            return Ok(());
        }

        let mut all_valid = true;

        for env_name in env_names {
            match backend.environment_exists(env_name).await {
                Ok(true) => {
                    if verbose {
                        info!("Environment {} is valid", env_name);
                    }
                }
                Ok(false) => {
                    warn!("Environment {} is missing", env_name);
                    all_valid = false;
                }
                Err(e) => {
                    error!("Error validating environment {}: {}", env_name, e);
                    all_valid = false;
                }
            }
        }

        if all_valid {
            info!("All environments are valid");
            Ok(())
        } else {
            warn!("Some environments are invalid or missing");
            Err(EnvError::Validation(
                "Environment validation failed".to_string(),
            ))
        }
    } else if let Some(ref name) = args.name {
        // Validate specific environment
        match backend.environment_exists(name).await {
            Ok(true) => {
                info!("Environment {} is valid", name);
                Ok(())
            }
            Ok(false) => {
                warn!("Environment {} is missing", name);
                Err(EnvError::Validation(format!(
                    "Environment {} validation failed",
                    name
                )))
            }
            Err(e) => {
                error!("Error validating environment {}: {}", name, e);
                Err(e)
            }
        }
    } else {
        Ok(())
    }
}

/// Execute environment installation
async fn execute_env_install(args: EnvInstallArgs, verbose: bool) -> Result<()> {
    info!("Installing packages in conda environment...");

    let backend = build_default_backend().await?;

    let env_name = args.name.as_deref().unwrap_or("xdxtools-core");

    let packages_to_install = parse_package_specs(&args.packages);

    if packages_to_install.is_empty() {
        return Err(EnvError::Validation(
            "No packages specified for installation".to_string(),
        ));
    }

    if verbose {
        info!("Installing packages in environment: {}", env_name);
        info!("Packages to install: {:?}", packages_to_install);
    }

    match backend
        .install_packages(
            env_name,
            &packages_to_install,
            execution_output_mode(verbose),
        )
        .await
    {
        Ok(_) => {
            info!("Successfully installed packages in {}", env_name);
            Ok(())
        }
        Err(e) => {
            error!("Failed to install packages: {}", e);
            Err(e)
        }
    }
}

/// Execute environment adoption
async fn execute_env_adopt(args: EnvAdoptArgs, verbose: bool) -> Result<()> {
    info!("Adopting environment into rattler ownership...");

    let backend = build_default_backend().await?;
    let target = match (args.name, args.prefix) {
        (Some(name), None) => crate::backend::EnvironmentTarget::Name(name),
        (None, Some(prefix)) => crate::backend::EnvironmentTarget::Prefix(prefix),
        _ => {
            return Err(EnvError::Validation(
                "Must specify exactly one of --name or --prefix".to_string(),
            ))
        }
    };

    backend
        .adopt_environment(&target, execution_output_mode(verbose))
        .await
}

/// Execute environment removal
async fn execute_env_remove(name: String, verbose: bool) -> Result<()> {
    info!("Removing conda environment: {}", name);

    let backend = build_default_backend().await?;

    match backend
        .remove_environment_with_output(&name, execution_output_mode(verbose))
        .await
    {
        Ok(_) => {
            info!("Successfully removed environment: {}", name);
            Ok(())
        }
        Err(e) => {
            error!("Failed to remove environment {}: {}", name, e);
            Err(e)
        }
    }
}

fn build_activation_path_entries(prefix: &Path) -> Vec<PathBuf> {
    let mut path_entries = Vec::new();

    #[cfg(not(target_os = "windows"))]
    {
        path_entries.push(prefix.join("bin"));
    }

    #[cfg(target_os = "windows")]
    {
        path_entries.push(prefix.join("Scripts"));
        path_entries.push(prefix.join("Library").join("bin"));
    }

    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    path_entries
}

fn build_activation_path(prefix: &Path) -> Result<String> {
    std::env::join_paths(build_activation_path_entries(prefix))
        .map(|value| value.to_string_lossy().into_owned())
        .map_err(|error| {
            EnvError::Environment(format!(
                "Failed to construct PATH for environment {}: {}",
                prefix.display(),
                error
            ))
        })
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn current_binary_path() -> Result<PathBuf> {
    std::env::current_exe().map_err(|error| {
        EnvError::Execution(format!(
            "Failed to determine the enva executable path for shell hook generation: {}",
            error
        ))
    })
}

fn render_shell_hook(shell: ActivationShell, binary_path: &Path) -> String {
    match shell.resolved() {
        ActivationShell::Bash | ActivationShell::Zsh => {
            let binary = sh_quote(&binary_path.to_string_lossy());
            format!(
                "export __ENVA_BIN={}\n\
                 enva() {{\n\
                   if [ \"$#\" -eq 0 ]; then\n\
                     \"$__ENVA_BIN\"\n\
                     return $?\n\
                   fi\n\
                   case \"$1\" in\n\
                     activate)\n\
                       shift\n\
                       eval \"$(\"$__ENVA_BIN\" activate \"$@\")\"\n\
                       ;;\n\
                     deactivate)\n\
                       shift\n\
                       eval \"$(\"$__ENVA_BIN\" deactivate \"$@\")\"\n\
                       ;;\n\
                     *)\n\
                       \"$__ENVA_BIN\" \"$@\"\n\
                       ;;\n\
                   esac\n\
                 }}\n",
                binary
            )
        }
        ActivationShell::Fish => {
            let binary = sh_quote(&binary_path.to_string_lossy());
            format!(
                "set -gx __ENVA_BIN {}\n\
                 function enva\n\
                   if test (count $argv) -eq 0\n\
                     $__ENVA_BIN\n\
                     return $status\n\
                   end\n\
                   switch $argv[1]\n\
                     case activate\n\
                       set -l rest $argv[2..-1]\n\
                       eval ($__ENVA_BIN activate $rest)\n\
                     case deactivate\n\
                       set -l rest $argv[2..-1]\n\
                       eval ($__ENVA_BIN deactivate $rest)\n\
                     case '*'\n\
                       $__ENVA_BIN $argv\n\
                   end\n\
                 end\n",
                binary
            )
        }
        ActivationShell::Powershell => {
            let binary = powershell_quote(&binary_path.to_string_lossy());
            format!(
                "$env:__ENVA_BIN = {}\n\
                 function global:enva {{\n\
                   param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Args)\n\
                   if ($Args.Count -eq 0) {{\n\
                     & $env:__ENVA_BIN\n\
                     return\n\
                   }}\n\
                   $command = $Args[0]\n\
                   $rest = @()\n\
                   if ($Args.Count -gt 1) {{\n\
                     $rest = $Args[1..($Args.Count - 1)]\n\
                   }}\n\
                   switch ($command) {{\n\
                     'activate' {{\n\
                       Invoke-Expression (& $env:__ENVA_BIN activate @rest)\n\
                       break\n\
                     }}\n\
                     'deactivate' {{\n\
                       Invoke-Expression (& $env:__ENVA_BIN deactivate @rest)\n\
                       break\n\
                     }}\n\
                     default {{\n\
                       & $env:__ENVA_BIN @Args\n\
                     }}\n\
                   }}\n\
                 }}\n",
                binary
            )
        }
        ActivationShell::Auto => unreachable!(),
    }
}

fn activation_env_name(prefix: &Path, requested_name: Option<&str>) -> String {
    requested_name
        .map(str::to_string)
        .or_else(|| {
            prefix
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| prefix.display().to_string())
}

fn render_posix_saved_var(name: &str, value: Option<String>) -> String {
    match value {
        Some(value) => format!(
            "export {}={}
",
            name,
            sh_quote(&value)
        ),
        None => format!(
            "unset {}
",
            name
        ),
    }
}

fn render_powershell_saved_var(name: &str, value: Option<String>) -> String {
    match value {
        Some(value) => format!(
            "$env:{} = {}
",
            name,
            powershell_quote(&value)
        ),
        None => format!(
            "Remove-Item Env:{} -ErrorAction SilentlyContinue
",
            name
        ),
    }
}

fn render_activation_script(
    shell: ActivationShell,
    prefix: &Path,
    env_name: &str,
) -> Result<String> {
    let shell = shell.resolved();
    let old_path = std::env::var("PATH").unwrap_or_default();
    let old_conda_prefix = std::env::var("CONDA_PREFIX").ok();
    let old_conda_default_env = std::env::var("CONDA_DEFAULT_ENV").ok();
    let old_conda_shlvl = std::env::var("CONDA_SHLVL").ok();
    let new_path = build_activation_path(prefix)?;
    let prefix_str = prefix.to_string_lossy().to_string();

    let script = match shell {
        ActivationShell::Bash | ActivationShell::Zsh => format!(
            "export ENVA_OLD_PATH={}
{}{}{}export PATH={}
export CONDA_PREFIX={}
export CONDA_DEFAULT_ENV={}
export CONDA_SHLVL='1'
export ENVA_ACTIVE_PREFIX={}
export ENVA_ACTIVE_NAME={}
hash -r 2>/dev/null || true
",
            sh_quote(&old_path),
            render_posix_saved_var("ENVA_OLD_CONDA_PREFIX", old_conda_prefix),
            render_posix_saved_var("ENVA_OLD_CONDA_DEFAULT_ENV", old_conda_default_env),
            render_posix_saved_var("ENVA_OLD_CONDA_SHLVL", old_conda_shlvl),
            sh_quote(&new_path),
            sh_quote(&prefix_str),
            sh_quote(env_name),
            sh_quote(&prefix_str),
            sh_quote(env_name),
        ),
        ActivationShell::Fish => {
            let path_entries = build_activation_path_entries(prefix)
                .into_iter()
                .map(|entry| sh_quote(&entry.to_string_lossy()))
                .collect::<Vec<String>>()
                .join(" ");
            format!(
                "set -gx ENVA_OLD_PATH $PATH
{}{}{}set -gx PATH {}
set -gx CONDA_PREFIX {}
set -gx CONDA_DEFAULT_ENV {}
set -gx CONDA_SHLVL 1
set -gx ENVA_ACTIVE_PREFIX {}
set -gx ENVA_ACTIVE_NAME {}
",
                if old_conda_prefix.is_some() {
                    "set -gx ENVA_OLD_CONDA_PREFIX $CONDA_PREFIX
"
                    .to_string()
                } else {
                    "set -e ENVA_OLD_CONDA_PREFIX
"
                    .to_string()
                },
                if old_conda_default_env.is_some() {
                    "set -gx ENVA_OLD_CONDA_DEFAULT_ENV $CONDA_DEFAULT_ENV
"
                    .to_string()
                } else {
                    "set -e ENVA_OLD_CONDA_DEFAULT_ENV
"
                    .to_string()
                },
                if old_conda_shlvl.is_some() {
                    "set -gx ENVA_OLD_CONDA_SHLVL $CONDA_SHLVL
"
                    .to_string()
                } else {
                    "set -e ENVA_OLD_CONDA_SHLVL
"
                    .to_string()
                },
                path_entries,
                sh_quote(&prefix_str),
                sh_quote(env_name),
                sh_quote(&prefix_str),
                sh_quote(env_name),
            )
        }
        ActivationShell::Powershell => format!(
            "$env:ENVA_OLD_PATH = {}
{}{}{}$env:PATH = {}
$env:CONDA_PREFIX = {}
$env:CONDA_DEFAULT_ENV = {}
$env:CONDA_SHLVL = '1'
$env:ENVA_ACTIVE_PREFIX = {}
$env:ENVA_ACTIVE_NAME = {}
",
            powershell_quote(&old_path),
            render_powershell_saved_var("ENVA_OLD_CONDA_PREFIX", old_conda_prefix),
            render_powershell_saved_var("ENVA_OLD_CONDA_DEFAULT_ENV", old_conda_default_env),
            render_powershell_saved_var("ENVA_OLD_CONDA_SHLVL", old_conda_shlvl),
            powershell_quote(&new_path),
            powershell_quote(&prefix_str),
            powershell_quote(env_name),
            powershell_quote(&prefix_str),
            powershell_quote(env_name),
        ),
        ActivationShell::Auto => unreachable!(),
    };

    Ok(script)
}

fn render_deactivation_script(shell: ActivationShell) -> String {
    match shell.resolved() {
        ActivationShell::Bash | ActivationShell::Zsh => "if [ \"${ENVA_OLD_PATH+x}\" = x ]; then export PATH=\"$ENVA_OLD_PATH\"; fi
unset ENVA_OLD_PATH
if [ \"${ENVA_OLD_CONDA_PREFIX+x}\" = x ]; then export CONDA_PREFIX=\"$ENVA_OLD_CONDA_PREFIX\"; else unset CONDA_PREFIX; fi
unset ENVA_OLD_CONDA_PREFIX
if [ \"${ENVA_OLD_CONDA_DEFAULT_ENV+x}\" = x ]; then export CONDA_DEFAULT_ENV=\"$ENVA_OLD_CONDA_DEFAULT_ENV\"; else unset CONDA_DEFAULT_ENV; fi
unset ENVA_OLD_CONDA_DEFAULT_ENV
if [ \"${ENVA_OLD_CONDA_SHLVL+x}\" = x ]; then export CONDA_SHLVL=\"$ENVA_OLD_CONDA_SHLVL\"; else unset CONDA_SHLVL; fi
unset ENVA_OLD_CONDA_SHLVL
unset ENVA_ACTIVE_PREFIX ENVA_ACTIVE_NAME
hash -r 2>/dev/null || true
".to_string(),
        ActivationShell::Fish => "if set -q ENVA_OLD_PATH
    set -gx PATH $ENVA_OLD_PATH
end
set -e ENVA_OLD_PATH
if set -q ENVA_OLD_CONDA_PREFIX
    set -gx CONDA_PREFIX $ENVA_OLD_CONDA_PREFIX
else
    set -e CONDA_PREFIX
end
set -e ENVA_OLD_CONDA_PREFIX
if set -q ENVA_OLD_CONDA_DEFAULT_ENV
    set -gx CONDA_DEFAULT_ENV $ENVA_OLD_CONDA_DEFAULT_ENV
else
    set -e CONDA_DEFAULT_ENV
end
set -e ENVA_OLD_CONDA_DEFAULT_ENV
if set -q ENVA_OLD_CONDA_SHLVL
    set -gx CONDA_SHLVL $ENVA_OLD_CONDA_SHLVL
else
    set -e CONDA_SHLVL
end
set -e ENVA_OLD_CONDA_SHLVL ENVA_ACTIVE_PREFIX ENVA_ACTIVE_NAME
".to_string(),
        ActivationShell::Powershell => "if (Test-Path Env:ENVA_OLD_PATH) { $env:PATH = $env:ENVA_OLD_PATH }
Remove-Item Env:ENVA_OLD_PATH -ErrorAction SilentlyContinue
if (Test-Path Env:ENVA_OLD_CONDA_PREFIX) { $env:CONDA_PREFIX = $env:ENVA_OLD_CONDA_PREFIX } else { Remove-Item Env:CONDA_PREFIX -ErrorAction SilentlyContinue }
Remove-Item Env:ENVA_OLD_CONDA_PREFIX -ErrorAction SilentlyContinue
if (Test-Path Env:ENVA_OLD_CONDA_DEFAULT_ENV) { $env:CONDA_DEFAULT_ENV = $env:ENVA_OLD_CONDA_DEFAULT_ENV } else { Remove-Item Env:CONDA_DEFAULT_ENV -ErrorAction SilentlyContinue }
Remove-Item Env:ENVA_OLD_CONDA_DEFAULT_ENV -ErrorAction SilentlyContinue
if (Test-Path Env:ENVA_OLD_CONDA_SHLVL) { $env:CONDA_SHLVL = $env:ENVA_OLD_CONDA_SHLVL } else { Remove-Item Env:CONDA_SHLVL -ErrorAction SilentlyContinue }
Remove-Item Env:ENVA_OLD_CONDA_SHLVL -ErrorAction SilentlyContinue
Remove-Item Env:ENVA_ACTIVE_PREFIX -ErrorAction SilentlyContinue
Remove-Item Env:ENVA_ACTIVE_NAME -ErrorAction SilentlyContinue
".to_string(),
        ActivationShell::Auto => unreachable!(),
    }
}

async fn execute_env_activate(args: EnvActivateArgs, verbose: bool) -> Result<()> {
    let requested_name = args.requested_env_name();
    if requested_name.is_none() && args.prefix.is_none() {
        return Err(EnvError::Validation(
            "Must specify an environment name or --prefix".to_string(),
        ));
    }

    let resolved = crate::env_run::resolve_environment_reference(
        requested_name.as_deref(),
        args.prefix.as_deref(),
        args.pm,
    )
    .await?;

    if !resolved.prefix.join("conda-meta").is_dir() {
        return Err(EnvError::Validation(format!(
            "Environment prefix is not a valid conda-style environment: {}",
            resolved.prefix.display()
        )));
    }

    let env_name = activation_env_name(&resolved.prefix, requested_name.as_deref());
    if verbose {
        info!(
            "Generating activation script for environment '{}' at {}",
            env_name,
            resolved.prefix.display()
        );
    }

    print!(
        "{}",
        render_activation_script(args.shell, &resolved.prefix, &env_name)?
    );
    Ok(())
}

async fn execute_env_deactivate(args: EnvDeactivateArgs, verbose: bool) -> Result<()> {
    if verbose {
        info!("Generating deactivation script");
    }

    print!("{}", render_deactivation_script(args.shell));
    Ok(())
}

async fn execute_env_shell(args: EnvShellArgs, verbose: bool) -> Result<()> {
    match args.command {
        EnvShellCommand::Hook(hook_args) => execute_env_shell_hook(hook_args, verbose).await,
    }
}

async fn execute_env_shell_hook(args: EnvShellHookArgs, verbose: bool) -> Result<()> {
    let binary_path = current_binary_path()?;
    if verbose {
        info!(
            "Generating shell hook for {} using {}",
            match args.shell.resolved() {
                ActivationShell::Bash => "bash",
                ActivationShell::Zsh => "zsh",
                ActivationShell::Fish => "fish",
                ActivationShell::Powershell => "powershell",
                ActivationShell::Auto => unreachable!(),
            },
            binary_path.display()
        );
    }

    print!("{}", render_shell_hook(args.shell, &binary_path));
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
struct GroupedEnvironmentPrefix {
    prefix: String,
    active: bool,
    owner: Option<String>,
    source: Option<String>,
    adopted_from: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct GroupedEnvironmentView {
    name: String,
    active: bool,
    primary_prefix: String,
    primary_owner: Option<String>,
    prefixes: Vec<GroupedEnvironmentPrefix>,
}

fn owner_priority_label(owner: Option<&str>) -> u8 {
    match owner {
        Some(owner) if owner.eq_ignore_ascii_case("rattler") => 0,
        Some(owner) if owner.eq_ignore_ascii_case("external") => 1,
        Some(_) => 2,
        None => 3,
    }
}

fn source_priority_label(source: Option<&str>) -> u8 {
    match source {
        Some(source) if source.eq_ignore_ascii_case("rattler") => 0,
        Some(source) if source.eq_ignore_ascii_case("micromamba") => 1,
        Some(source) if source.eq_ignore_ascii_case("mamba") => 2,
        Some(source) if source.eq_ignore_ascii_case("conda") => 3,
        Some(_) => 4,
        None => 5,
    }
}

fn group_conda_environments(environments: Vec<CondaEnvironment>) -> Vec<GroupedEnvironmentView> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<String, Vec<CondaEnvironment>> = BTreeMap::new();
    for environment in environments {
        grouped
            .entry(environment.name.clone())
            .or_default()
            .push(environment);
    }

    grouped
        .into_iter()
        .map(|(name, mut entries)| {
            entries.sort_by(|left, right| {
                owner_priority_label(left.owner.as_deref())
                    .cmp(&owner_priority_label(right.owner.as_deref()))
                    .then_with(|| {
                        source_priority_label(left.source.as_deref())
                            .cmp(&source_priority_label(right.source.as_deref()))
                    })
                    .then_with(|| right.is_active.cmp(&left.is_active))
                    .then(left.prefix.cmp(&right.prefix))
            });

            let prefixes = entries
                .iter()
                .map(|entry| GroupedEnvironmentPrefix {
                    prefix: entry.prefix.clone(),
                    active: entry.is_active,
                    owner: entry.owner.clone(),
                    source: entry.source.clone(),
                    adopted_from: entry.adopted_from.clone(),
                })
                .collect::<Vec<GroupedEnvironmentPrefix>>();

            GroupedEnvironmentView {
                name,
                active: entries.iter().any(|entry| entry.is_active),
                primary_prefix: entries
                    .first()
                    .map(|entry| entry.prefix.clone())
                    .unwrap_or_default(),
                primary_owner: entries.first().and_then(|entry| entry.owner.clone()),
                prefixes,
            }
        })
        .collect()
}

/// List all conda environments (not just enva-managed ones)
///
/// This function displays all conda environments in the system,
/// showing merged same-name environments and their prefix paths.
async fn list_all_conda_environments(detailed: bool, json: bool) -> Result<()> {
    let backend = build_default_backend().await?;
    let grouped = group_conda_environments(backend.get_all_conda_environments().await?);

    if json {
        let output = serde_json::json!({
            "environments": grouped,
            "count": grouped.len()
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if grouped.is_empty() {
        info!("No conda environments found");
        return Ok(());
    }

    println!();
    if detailed {
        println!(
            "{:<30} | {:<10} | {:<12} | {:<12} | {}",
            "Name", "Owner", "Source", "Adopted From", "Prefix"
        );
        println!("{}", "-".repeat(120));

        for environment in &grouped {
            for (index, prefix) in environment.prefixes.iter().enumerate() {
                let name = if index == 0 {
                    format!(
                        "{}{}",
                        environment.name,
                        if environment.active { "*" } else { "" }
                    )
                } else {
                    String::new()
                };
                println!(
                    "{:<30} | {:<10} | {:<12} | {:<12} | {}",
                    name,
                    prefix.owner.as_deref().unwrap_or("unknown"),
                    prefix.source.as_deref().unwrap_or("unknown"),
                    prefix.adopted_from.as_deref().unwrap_or("-"),
                    prefix.prefix
                );
            }
        }
    } else {
        println!("{:<30} | {:<10} | {}", "Name", "Owner", "Prefixes");
        println!("{}", "-".repeat(120));

        for environment in &grouped {
            let joined = environment
                .prefixes
                .iter()
                .map(|prefix| prefix.prefix.as_str())
                .collect::<Vec<&str>>()
                .join(" ; ");
            println!(
                "{:<30} | {:<10} | {}",
                format!(
                    "{}{}",
                    environment.name,
                    if environment.active { "*" } else { "" }
                ),
                environment.primary_owner.as_deref().unwrap_or("unknown"),
                joined
            );
        }
    }
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        group_conda_environments, owner_priority_label, parse_package_specs,
        render_activation_script, render_deactivation_script, render_shell_hook,
        source_priority_label, ActivationShell,
    };
    use crate::micromamba::CondaEnvironment;
    use std::path::Path;

    fn environment(
        name: &str,
        prefix: &str,
        active: bool,
        owner: Option<&str>,
        source: Option<&str>,
        adopted_from: Option<&str>,
    ) -> CondaEnvironment {
        CondaEnvironment {
            name: name.to_string(),
            prefix: prefix.to_string(),
            is_active: active,
            source: source.map(str::to_string),
            owner: owner.map(str::to_string),
            adopted_from: adopted_from.map(str::to_string),
        }
    }

    #[test]
    fn owner_priority_prefers_rattler_before_external() {
        assert!(owner_priority_label(Some("rattler")) < owner_priority_label(Some("external")));
    }

    #[test]
    fn source_priority_prefers_rattler_then_micromamba_then_mamba_then_conda() {
        assert!(source_priority_label(Some("rattler")) < source_priority_label(Some("micromamba")));
        assert!(source_priority_label(Some("micromamba")) < source_priority_label(Some("mamba")));
        assert!(source_priority_label(Some("mamba")) < source_priority_label(Some("conda")));
    }

    #[test]
    fn group_conda_environments_merges_same_name_and_sorts_prefixes_by_priority() {
        let grouped = group_conda_environments(vec![
            environment(
                "demo",
                "/conda/demo",
                false,
                Some("external"),
                Some("conda"),
                None,
            ),
            environment(
                "demo",
                "/mamba/demo",
                false,
                Some("external"),
                Some("mamba"),
                None,
            ),
            environment(
                "demo",
                "/rattler/demo",
                false,
                Some("rattler"),
                Some("rattler"),
                None,
            ),
            environment(
                "demo",
                "/micromamba/demo",
                true,
                Some("external"),
                Some("micromamba"),
                None,
            ),
            environment(
                "other",
                "/other",
                false,
                Some("external"),
                Some("conda"),
                None,
            ),
        ]);

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].name, "demo");
        assert!(grouped[0].active);
        assert_eq!(grouped[0].primary_prefix, "/rattler/demo");
        assert_eq!(
            grouped[0]
                .prefixes
                .iter()
                .map(|prefix| prefix.prefix.as_str())
                .collect::<Vec<&str>>(),
            vec![
                "/rattler/demo",
                "/micromamba/demo",
                "/mamba/demo",
                "/conda/demo"
            ]
        );
    }

    #[test]
    fn parse_package_specs_splits_commas_and_discards_empty_values() {
        assert_eq!(
            parse_package_specs(&[
                "fastqc,multiqc".to_string(),
                "  ".to_string(),
                "seqtk".to_string(),
            ]),
            vec!["fastqc", "multiqc", "seqtk"]
        );
    }

    #[test]
    fn render_activation_script_for_bash_exports_expected_variables() {
        let script =
            render_activation_script(ActivationShell::Bash, Path::new("/tmp/demo"), "demo")
                .unwrap();

        assert!(script.contains("export PATH='"));
        assert!(script.contains("export CONDA_PREFIX='/tmp/demo'"));
        assert!(script.contains("export CONDA_DEFAULT_ENV='demo'"));
        assert!(script.contains("export ENVA_ACTIVE_NAME='demo'"));
    }

    #[test]
    fn render_deactivation_script_for_bash_unsets_activation_state() {
        let script = render_deactivation_script(ActivationShell::Bash);

        assert!(script.contains("unset ENVA_OLD_PATH"));
        assert!(script.contains("unset ENVA_ACTIVE_PREFIX ENVA_ACTIVE_NAME"));
    }

    #[test]
    fn render_shell_hook_for_bash_wraps_activate_and_deactivate() {
        let script = render_shell_hook(ActivationShell::Bash, Path::new("/tmp/enva"));

        assert!(script.contains("export __ENVA_BIN='/tmp/enva'"));
        assert!(script.contains("enva() {"));
        assert!(script.contains("eval \"$(\"$__ENVA_BIN\" activate \"$@\")\""));
        assert!(script.contains("eval \"$(\"$__ENVA_BIN\" deactivate \"$@\")\""));
        assert!(script.contains("\"$__ENVA_BIN\" \"$@\""));
    }
}
