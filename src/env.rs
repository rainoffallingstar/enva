//! Environment management commands

use crate::error::{Result, EnvError};
use crate::micromamba::MicromambaManager;
use clap::{Args, Subcommand};
use std::path::PathBuf;
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

    /// Remove conda environment
    Remove {
        /// Environment name
        name: String,
    },

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
        EnvCommand::Remove { name } => execute_env_remove(name, verbose).await,
        EnvCommand::Run(args) => crate::env_run::execute_env_run(args, verbose).await,
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

    let micromamba_manager = MicromambaManager::get_global_manager().await.map_err(|e| {
        error!("Failed to initialize MicromambaManager: {}", e);
        EnvError::Execution(
            "Micromamba not found and auto-install failed.".to_string(),
        )
    })?;

    // Determine which environments to create
    let mut environments_to_create = Vec::new();

    // If custom YAML is specified, create single environment from that YAML
    if args.yaml.is_some() {
        // For custom YAML, use the name from YAML file or --name parameter
        if let Some(ref name) = args.name {
            environments_to_create.push(name.as_str());
        } else {
            return Err(EnvError::Validation(
                "When using --yaml, you must also specify --name for the environment".to_string(),
            ));
        }
    } else {
        // Standard environment creation logic
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
    }

    if dry_run {
        // Enhanced dry-run output with detailed debugging
        use serde_json::{json, Value};
        let mut results = Vec::new();

        for env_name in &environments_to_create {
            // Determine YAML file path (same logic as actual execution)
            let yaml_file = if let Some(ref yaml_path) = args.yaml {
                yaml_path.clone()
            } else {
                // Use default path: try multiple locations (same as actual execution)
                let current_dir = std::env::current_dir().unwrap_or_default();

                // Try src/configs/ first (development)
                let src_config = current_dir
                    .join("src")
                    .join("configs")
                    .join(format!("{}.yaml", env_name));

                // Try environments/configs/ second (release)
                let envs_config = current_dir
                    .join("environments")
                    .join("configs")
                    .join(format!("{}.yaml", env_name));

                // Return whichever exists, prefer src/configs/
                if src_config.exists() {
                    src_config
                } else if envs_config.exists() {
                    envs_config
                } else {
                    // Return src path as default (will show as not found)
                    src_config
                }
            };

            // Check if file exists
            let file_exists = yaml_file.exists();
            let file_path_str = yaml_file.to_string_lossy().to_string();

            if json {
                results.push(json!({
                    "environment": env_name,
                    "yaml_file": file_path_str,
                    "file_exists": file_exists,
                    "action": "create",
                    "dry_run": true,
                    "status": if file_exists { "ready" } else { "file_not_found" }
                }));
            } else {
                // Text output for dry-run
                println!("[DRY-RUN] Environment: {}", env_name);
                println!("[DRY-RUN] YAML file: {}", file_path_str);
                println!("[DRY-RUN] File exists: {}", if file_exists { "YES" } else { "NO" });
                println!("[DRY-RUN] Status: {}", if file_exists { "Ready to create" } else { "File not found!" });
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

    for env_name in environments_to_create {
        // Lock manager for this iteration
        let manager = micromamba_manager.lock().await;

        // Determine YAML file to use
        let yaml_file = if let Some(ref yaml_path) = args.yaml {
            // Use custom YAML file
            yaml_path.clone()
        } else {
            // Use default path: try multiple locations
            let current_dir = std::env::current_dir().map_err(|e| {
                error!("Failed to get current directory: {}", e);
                EnvError::FileOperation(format!(
                    "Failed to get current directory: {}",
                    e
                ))
            })?;

            // Try src/configs/ first (development)
            let src_config = current_dir
                .join("src")
                .join("configs")
                .join(format!("{}.yaml", env_name));

            // Try environments/configs/ second (release)
            let envs_config = current_dir
                .join("environments")
                .join("configs")
                .join(format!("{}.yaml", env_name));

            // Return whichever exists, prefer src/configs/
            if src_config.exists() {
                src_config
            } else if envs_config.exists() {
                envs_config
            } else {
                // Return src path as default (will fail with proper error)
                src_config
            }
        };

        match manager
            .create_environment(&yaml_file, dry_run)
            .await
        {
            Ok(_) => {
                success_count += 1;
                info!("Successfully created environment: {}", env_name);
            }
            Err(e) => {
                failed_count += 1;
                error!("Failed to create environment {}: {}", env_name, e);
            }
        }
    }

    info!(
        "Environment creation complete: {} succeeded, {} failed",
        success_count, failed_count
    );

    if failed_count > 0 {
        return Err(EnvError::Execution(format!(
            "{} environments failed to create",
            failed_count
        )));
    }

    Ok(())
}

/// Execute environment list
async fn execute_env_list(args: EnvListArgs, _verbose: bool, json: bool) -> Result<()> {
    info!("Listing conda environments...");

    // 直接显示所有 conda 环境
    list_all_conda_environments(json).await
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

    let micromamba_manager = MicromambaManager::get_global_manager().await.map_err(|e| {
        error!("Failed to initialize MicromambaManager: {}", e);
        EnvError::Execution(
            "Micromamba not found and auto-install failed.".to_string(),
        )
    })?;

    let manager = micromamba_manager.lock().await;

    if args.all || args.name.is_none() {
        // Validate all environments by checking if they exist
        let env_names = vec![
            "xdxtools-core",
            "xdxtools-snakemake",
            "xdxtools-extra",
        ];

        if json {
            use serde_json::{json, Value};
            let mut results = Vec::new();

            for env_name in env_names {
                let exists = manager
                    .environment_exists(env_name)
                    .await
                    .unwrap_or(false);
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
            match manager.environment_exists(env_name).await {
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
        match manager.environment_exists(name).await {
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

    let micromamba_manager = MicromambaManager::get_global_manager().await.map_err(|e| {
        error!("Failed to initialize MicromambaManager: {}", e);
        EnvError::Execution(
            "Micromamba not found and auto-install failed.".to_string(),
        )
    })?;

    let manager = micromamba_manager.lock().await;
    let env_name = args.name.as_deref().unwrap_or("xdxtools-core");

    // Flatten and parse package list (support comma-separated)
    let mut packages_to_install = Vec::new();
    for pkg_list in &args.packages {
        for pkg in pkg_list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            packages_to_install.push(pkg.to_string());
        }
    }

    if packages_to_install.is_empty() {
        return Err(EnvError::Validation(
            "No packages specified for installation".to_string(),
        ));
    }

    if verbose {
        info!("Installing packages in environment: {}", env_name);
        info!("Packages to install: {:?}", packages_to_install);
    }

    match manager.install_packages(env_name, &packages_to_install).await {
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

/// Execute environment removal
async fn execute_env_remove(name: String, verbose: bool) -> Result<()> {
    info!("Removing conda environment: {}", name);

    let micromamba_manager = MicromambaManager::get_global_manager().await.map_err(|e| {
        error!("Failed to initialize MicromambaManager: {}", e);
        EnvError::Execution(
            "Micromamba not found and auto-install failed.".to_string(),
        )
    })?;

    let manager = micromamba_manager.lock().await;

    match manager.remove_environment(&name).await {
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

/// List all conda environments (not just enva-managed ones)
///
/// This function displays all conda environments in the system,
/// showing their names and prefix paths.
async fn list_all_conda_environments(json: bool) -> Result<()> {
    use crate::micromamba::MicromambaManager;

    // Get the global manager
    let micromamba_manager = MicromambaManager::get_global_manager().await.map_err(|e| {
        error!("Failed to initialize MicromambaManager: {}", e);
        EnvError::Execution(
            "Micromamba not found and auto-install failed.".to_string(),
        )
    })?;

    let manager = micromamba_manager.lock().await;

    // Get all conda environments
    let environments = manager.get_all_conda_environments().await?;

    if json {
        // JSON output
        use serde_json::Value;
        let env_array: Vec<Value> = environments
            .iter()
            .map(|env| {
                serde_json::json!({
                    "name": env.name,
                    "prefix": env.prefix,
                    "active": env.is_active,
                })
            })
            .collect();

        let output = serde_json::json!({
            "environments": env_array,
            "count": environments.len()
        });

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Table format
        if environments.is_empty() {
            info!("No conda environments found");
            return Ok(());
        }

        println!();
        println!("{:<30} | {}", "Name", "Prefix");
        println!("{}", "-".repeat(100));

        for env in &environments {
            let active_mark = if env.is_active { "*" } else { "" };
            println!("{:<30} | {}", format!("{}{}", env.name, active_mark), env.prefix);
        }
        println!();
    }

    Ok(())
}
