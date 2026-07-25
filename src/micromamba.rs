//! Compatibility package-manager support for enva.
//!
//! enva is rattler-first. This module retains the historical `MicromambaManager`
//! type as a compatibility layer for `micromamba` / `mamba` / `conda` discovery,
//! adoption, and explicit fallback flows.

use crate::backend::{
    append_environment_run_command, append_environment_shell_arguments, EnvironmentName,
    OutputMode, RunCommand,
};
use crate::error::{EnvError, Result};
use crate::ownership::ownership_record_path;
use crate::package_manager::{PackageManager, PackageManagerDetector};
use crate::{BUILT_IN_ENV_NAMES, CORE_ENV_NAME, EXTRA_ENV_NAME, SNAKEMAKE_ENV_NAME};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::Duration;
use tokio::process::Command as AsyncCommand;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use which::which;

/// Global micromamba manager instance (lazy initialization)
static GLOBAL_MANAGER: LazyLock<Mutex<Option<MicromambaManager>>> =
    LazyLock::new(|| Mutex::new(None));

/// Track whether global manager has been initialized (to control logging)
static GLOBAL_INITIALIZED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

/// Shared runtime managers for fast env resolution paths
static RUNTIME_MANAGER_CACHE: LazyLock<StdMutex<HashMap<PackageManager, MicromambaManager>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

const CAPTURED_FAILURE_OUTPUT_LIMIT_BYTES: usize = 16 * 1024;

fn captured_output_tail(output: &[u8]) -> String {
    let start_index: usize = output
        .len()
        .saturating_sub(CAPTURED_FAILURE_OUTPUT_LIMIT_BYTES);
    let content: String = String::from_utf8_lossy(&output[start_index..])
        .trim()
        .to_string();
    if content.is_empty() {
        return String::new();
    }
    if start_index > 0 {
        return format!(
            "[truncated to last {CAPTURED_FAILURE_OUTPUT_LIMIT_BYTES} bytes]\n{content}"
        );
    }
    content
}

fn captured_command_failure_detail(output: &Output) -> String {
    let stderr: String = captured_output_tail(&output.stderr);
    if !stderr.is_empty() {
        return format!("; stderr: {stderr}");
    }

    let stdout: String = captured_output_tail(&output.stdout);
    if !stdout.is_empty() {
        return format!("; stdout: {stdout}");
    }

    String::new()
}

fn validate_micromamba_executable(path: &Path) -> Result<PathBuf> {
    let canonical_path = normalize_and_validate_path(path)?;
    let output = Command::new(&canonical_path)
        .arg("--version")
        .output()
        .map_err(|error| {
            EnvError::Execution(format!(
                "Failed to execute micromamba at {}: {}",
                canonical_path.display(),
                error
            ))
        })?;
    if !output.status.success() {
        return Err(EnvError::Execution(format!(
            "Micromamba at {} failed its version check with status {:?}: {}",
            canonical_path.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(canonical_path)
}

fn resolve_micromamba_path(
    explicitly_configured_path: Option<PathBuf>,
    path_discovery_result: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(configured_path) = explicitly_configured_path {
        return validate_micromamba_executable(&configured_path).map_err(|error| {
            EnvError::Config(format!(
                "ENVA_MICROMAMBA_PATH={} is not a usable micromamba executable: {}",
                configured_path.display(),
                error
            ))
        });
    }

    if let Some(discovered_path) = path_discovery_result {
        return validate_micromamba_executable(&discovered_path);
    }

    Err(EnvError::Config(
        "micromamba is an optional compatibility backend and is not installed; install it explicitly and add it to PATH, or set ENVA_MICROMAMBA_PATH"
            .to_string(),
    ))
}

/// Tool to environment mapping (updated for micromamba)
pub const TOOL_ENVIRONMENT_MAP: &[(&str, &str)] = &[
    // QC Tools -> otter-core
    ("fastqc", CORE_ENV_NAME),
    ("multiqc", CORE_ENV_NAME),
    ("seqkit", CORE_ENV_NAME),
    ("seqtk", CORE_ENV_NAME),
    ("samtools", CORE_ENV_NAME),
    ("picard", CORE_ENV_NAME),
    // Methylation Tools -> otter-core
    ("bismark", CORE_ENV_NAME),
    ("trim_galore", CORE_ENV_NAME),
    ("trim-galore", CORE_ENV_NAME),
    // RNA-seq Tools -> otter-core
    ("star", CORE_ENV_NAME),
    ("htseq-count", CORE_ENV_NAME),
    ("htseq", CORE_ENV_NAME),
    ("rmats", CORE_ENV_NAME),
    // ChIP-seq/ATAC-seq Core Tools -> otter-core
    ("macs2", CORE_ENV_NAME),
    ("bwa", CORE_ENV_NAME),
    ("bowtie2", CORE_ENV_NAME),
    ("bwa-index", CORE_ENV_NAME),     // BWA index building
    ("bowtie2-build", CORE_ENV_NAME), // Bowtie2 index building
    // Qualimap -> otter-core
    ("qualimap", CORE_ENV_NAME),
    // Snakemake -> otter-snakemake
    ("snakemake", SNAKEMAKE_ENV_NAME),
    ("jinja2", SNAKEMAKE_ENV_NAME),
    ("click", SNAKEMAKE_ENV_NAME),
    ("git", SNAKEMAKE_ENV_NAME),
    // Advanced Bioinformatics -> otter-extra
    ("bedtools", EXTRA_ENV_NAME),
    ("bcftools", EXTRA_ENV_NAME),
    ("vcftools", EXTRA_ENV_NAME),
    ("tabix", EXTRA_ENV_NAME),
    // Advanced ChIP-seq/ATAC-seq Tools -> otter-extra
    ("deepTools", EXTRA_ENV_NAME),
    ("genrich", EXTRA_ENV_NAME),
    ("homer", EXTRA_ENV_NAME),
    // Data Science & Visualization -> otter-extra
    ("jupyter", EXTRA_ENV_NAME),
    ("jupyterlab", EXTRA_ENV_NAME),
    ("flask", EXTRA_ENV_NAME),
    ("dash", EXTRA_ENV_NAME),
    ("streamlit", EXTRA_ENV_NAME),
    ("scikit-learn", EXTRA_ENV_NAME),
    ("scipy", EXTRA_ENV_NAME),
    ("statsmodels", EXTRA_ENV_NAME),
    // Development Toolchain -> otter-extra
    ("go", EXTRA_ENV_NAME),
    ("gofmt", EXTRA_ENV_NAME),
    ("rust", EXTRA_ENV_NAME),
    ("rustc", EXTRA_ENV_NAME),
    ("cargo", EXTRA_ENV_NAME),
];

/// Micromamba environment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicromambaEnvironment {
    /// Environment name
    pub name: String,
    /// Environment file path
    pub file_path: PathBuf,
    /// Tools available in this environment
    pub tools: Vec<String>,
    /// Environment status
    pub status: EnvironmentStatus,
    /// Creation timestamp
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Environment status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EnvironmentStatus {
    /// Environment exists and is ready
    Ready,
    /// Environment exists but needs verification
    Installed,
    /// Environment file exists but environment not created
    NotInstalled,
    /// Environment file not found
    Missing,
    /// Error checking environment
    Error(String),
}

/// Validation details for dry-run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationDetails {
    pub syntax_valid: bool,
    pub dependencies_resolvable: bool,
    pub version_conflicts: Vec<String>,
    pub channels_accessible: bool,
}

/// Validation result for dry-run mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub dry_run: bool,
    pub environment: String,
    pub yaml_file: PathBuf,
    pub validation: ValidationDetails,
    pub estimated_packages: usize,
    pub estimated_size_mb: u64,
    pub channels_accessible: Vec<String>,
}

/// Version configuration for environments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionConfig {
    /// Python version for environments
    pub python_version: String,
    /// R version for R environment
    pub r_version: String,
}

impl Default for VersionConfig {
    fn default() -> Self {
        Self {
            python_version: "3.10.13".to_string(), // Compatible with glibc 2.17
            r_version: "4.4.3".to_string(),
        }
    }
}

/// Micromamba manager for simplified environment handling
pub struct MicromambaManager {
    /// Path to package manager executable (conda/mamba/micromamba)
    pm_path: PathBuf,
    /// Detected package manager type
    pm_type: PackageManager,
    /// Environment configurations
    environments: HashMap<String, MicromambaEnvironment>,
    /// Base directory for environment files
    config_dir: PathBuf,
    /// Version configuration
    version_config: VersionConfig,
    /// Mutex for environment creation to prevent race conditions
    creation_lock: Arc<Mutex<()>>,
    /// Cached environment prefixes for this package manager instance
    env_list_cache: Arc<StdMutex<Option<Vec<PathBuf>>>>,
}

impl Clone for MicromambaManager {
    fn clone(&self) -> Self {
        Self {
            pm_path: self.pm_path.clone(),
            pm_type: self.pm_type,
            environments: self.environments.clone(),
            config_dir: self.config_dir.clone(),
            version_config: self.version_config.clone(),
            // Reuse the same lock across clones to maintain synchronization
            creation_lock: Arc::clone(&self.creation_lock),
            env_list_cache: Arc::clone(&self.env_list_cache),
        }
    }
}

/// Normalize and validate a micromamba path
fn normalize_and_validate_path(path: &Path) -> Result<PathBuf> {
    // Normalize the path (resolve relative paths, symlinks, etc.)
    let canonicalized = path.canonicalize().map_err(|e| {
        EnvError::FileOperation(format!("Failed to normalize path {:?}: {}", path, e))
    })?;

    // Check if path exists
    if !canonicalized.exists() {
        return Err(EnvError::FileOperation(format!(
            "Path does not exist: {:?}",
            canonicalized
        )));
    }

    // Check if it's a file (not a directory)
    if !canonicalized.is_file() {
        return Err(EnvError::FileOperation(format!(
            "Path is not a file: {:?}",
            canonicalized
        )));
    }

    // Check if we have execute permissions
    let metadata = fs::metadata(&canonicalized).map_err(|e| {
        EnvError::FileOperation(format!(
            "Failed to get metadata for {:?}: {}",
            canonicalized, e
        ))
    })?;

    #[cfg(unix)]
    {
        let permissions = metadata.permissions();
        if permissions.mode() & 0o111 == 0 {
            return Err(EnvError::PermissionDenied(format!(
                "No execute permission for: {:?}",
                canonicalized
            )));
        }
    }

    Ok(canonicalized)
}

impl MicromambaManager {
    /// Create new manager with automatic package manager detection
    /// Detects and uses: micromamba → mamba → conda (in priority order)
    pub async fn new() -> Result<Self> {
        let mut detector = PackageManagerDetector::new();
        let pm_type = detector.detect_with_env_override()?;
        Self::new_with_package_manager(pm_type).await
    }

    async fn build_manager(pm_type: PackageManager, initialize_envs: bool) -> Result<Self> {
        let pm_path = match pm_type {
            PackageManager::Conda | PackageManager::Mamba => which(pm_type.command())
                .map_err(|_| EnvError::Config(format!("{} not found in PATH", pm_type)))?,
            PackageManager::Micromamba => Self::find_micromamba()?,
            PackageManager::None => {
                return Err(EnvError::Config(
                    "No package manager found (conda/mamba/micromamba)".to_string(),
                ));
            }
        };

        info!("✓ Package manager: {} ({})", pm_type, pm_path.display());

        let config_dir = Self::get_cache_config_dir()?;
        let mut manager = Self {
            pm_path,
            pm_type,
            environments: HashMap::new(),
            config_dir,
            version_config: VersionConfig::default(),
            creation_lock: Arc::new(Mutex::new(())),
            env_list_cache: Arc::new(StdMutex::new(None)),
        };

        if initialize_envs {
            manager.initialize_environments(false).await?;
        }

        Ok(manager)
    }

    /// Create new manager bound to a specific package manager
    pub async fn new_with_package_manager(pm_type: PackageManager) -> Result<Self> {
        Self::build_manager(pm_type, true).await
    }

    /// Create a runtime-only manager that skips environment template initialization
    pub async fn new_runtime_with_package_manager(pm_type: PackageManager) -> Result<Self> {
        if let Ok(cache) = RUNTIME_MANAGER_CACHE.lock() {
            if let Some(manager) = cache.get(&pm_type) {
                return Ok(manager.clone());
            }
        }

        let manager = Self::build_manager(pm_type, false).await?;

        if let Ok(mut cache) = RUNTIME_MANAGER_CACHE.lock() {
            let cached = cache.entry(pm_type).or_insert_with(|| manager.clone());
            return Ok(cached.clone());
        }

        Ok(manager)
    }

    fn invalidate_runtime_manager_cache(pm_type: PackageManager) {
        if let Ok(cache) = RUNTIME_MANAGER_CACHE.lock() {
            if let Some(manager) = cache.get(&pm_type) {
                manager.invalidate_environment_list_cache();
            }
        }
    }

    /// Get detected package manager
    pub fn get_package_manager(&self) -> PackageManager {
        self.pm_type
    }

    /// Get package manager path
    pub fn get_pm_path(&self) -> &Path {
        &self.pm_path
    }

    /// Get cache directory for configuration files
    fn get_cache_config_dir() -> Result<PathBuf> {
        // Use XDG cache directory if available, otherwise fallback to temp directory
        if let Some(cache_dir) = dirs::cache_dir() {
            Ok(cache_dir.join("xdxtools").join("configs"))
        } else {
            // Fallback to temporary directory
            Ok(std::env::temp_dir().join("xdxtools").join("configs"))
        }
    }

    /// Get or create global micromamba manager instance
    /// This method implements lazy initialization and caching
    pub async fn get_global_manager() -> Result<Arc<Mutex<Self>>> {
        let mut global = GLOBAL_MANAGER.lock().await;
        let mut initialized = GLOBAL_INITIALIZED.lock().await;

        if global.is_none() {
            // Only show initialization logs on first run
            info!("Initializing global micromamba manager...");
            let manager = Self::new().await?;
            *global = Some(manager);
            *initialized = true;
            info!("Global micromamba manager initialized successfully");
        }

        // Clone the manager to return a new Arc<Mutex<Self>>
        // The Arc allows shared ownership and Mutex ensures exclusive access
        let manager = Arc::new(Mutex::new(global.as_ref().unwrap().clone()));
        Ok(manager)
    }

    /// Create micromamba manager with custom config directory
    pub async fn with_config_dir<P: AsRef<Path>>(config_dir: P) -> Result<Self> {
        let pm_path = Self::find_micromamba()?;
        let config_dir = config_dir.as_ref().to_path_buf();

        let mut manager = Self {
            pm_path,
            pm_type: PackageManager::Micromamba,
            environments: HashMap::new(),
            config_dir,
            version_config: VersionConfig::default(),
            creation_lock: Arc::new(Mutex::new(())),
            env_list_cache: Arc::new(StdMutex::new(None)),
        };

        manager.initialize_environments(true).await?;
        Ok(manager)
    }

    /// Create micromamba manager with custom version configuration
    pub async fn with_version_config<P: AsRef<Path>>(
        config_dir: P,
        version_config: VersionConfig,
    ) -> Result<Self> {
        let pm_path = Self::find_micromamba()?;
        let config_dir = config_dir.as_ref().to_path_buf();

        let mut manager = Self {
            pm_path,
            pm_type: PackageManager::Micromamba,
            environments: HashMap::new(),
            config_dir,
            version_config,
            creation_lock: Arc::new(Mutex::new(())),
            env_list_cache: Arc::new(StdMutex::new(None)),
        };

        manager.initialize_environments(true).await?;
        Ok(manager)
    }

    /// Get the cache directory path (for logging/debugging)
    pub fn get_cache_dir(&self) -> &PathBuf {
        &self.config_dir
    }

    fn null_device_path() -> &'static str {
        if cfg!(windows) {
            "NUL"
        } else {
            "/dev/null"
        }
    }

    /// Build environment variables for package-manager subprocess execution
    fn build_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        if self.pm_type != PackageManager::Micromamba {
            return env_vars;
        }

        let pm_dir = self
            .pm_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.pm_path.clone());

        let mamba_root = self.get_mamba_root_prefix();

        let lib_dir = pm_dir.join("lib");
        let existing_ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        let new_ld_path = if existing_ld_path.is_empty() {
            lib_dir.to_string_lossy().to_string()
        } else {
            format!("{}:{}", lib_dir.to_string_lossy(), existing_ld_path)
        };
        env_vars.insert("LD_LIBRARY_PATH".to_string(), new_ld_path);

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", pm_dir.to_string_lossy(), existing_path);
        env_vars.insert("PATH".to_string(), new_path);

        let mamba_root_str = mamba_root.to_string_lossy().to_string();
        env_vars.insert("_CONDA_ROOT".to_string(), mamba_root_str.clone());
        env_vars.insert("MAMBA_ROOT_PREFIX".to_string(), mamba_root_str.clone());
        env_vars.insert("CONDA_PREFIX".to_string(), mamba_root_str.clone());
        env_vars.insert("CONDA_DEFAULT_ENV".to_string(), "base".to_string());
        env_vars.insert("CONDARC".to_string(), Self::null_device_path().to_string());
        env_vars.insert("MAMBARC".to_string(), Self::null_device_path().to_string());

        env_vars
    }

    fn stash_ownership_marker(prefix: &Path) -> Result<Option<String>> {
        let marker_path = ownership_record_path(prefix);
        if !marker_path.is_file() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&marker_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to read ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })?;

        fs::remove_file(&marker_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to temporarily remove ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })?;

        Ok(Some(contents))
    }

    fn restore_ownership_marker(prefix: &Path, contents: Option<String>) -> Result<()> {
        let Some(contents) = contents else {
            return Ok(());
        };

        if !prefix.exists() {
            return Ok(());
        }

        let marker_path = ownership_record_path(prefix);
        fs::write(&marker_path, contents).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to restore ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })
    }

    /// Get MAMBA_ROOT_PREFIX path
    fn get_mamba_root_prefix(&self) -> PathBuf {
        // First check if environment variable is already set
        if let Ok(prefix) = std::env::var("MAMBA_ROOT_PREFIX") {
            return PathBuf::from(prefix);
        }

        // Check multiple possible locations for micromamba root
        // Priority: 1) ~/.local/share/mamba, 2) parent of bin directory, 3) ~/.local/share/mamba fallback

        // First check ~/.local/share/mamba (most common for user-installed micromamba)
        if let Some(home) = dirs::home_dir() {
            let local_mamba = home.join(".local/share/mamba");
            if local_mamba.exists() {
                return local_mamba;
            }
        }

        // Check if this looks like a standard micromamba installation
        // Standard path: /path/to/bin/micromamba with root at /path/to/share/mamba
        if let Some(dir) = self.pm_path.parent() {
            let potential_root = dir.join("share/mamba");
            if potential_root.exists() {
                return potential_root;
            }
            // Also check if there's an 'envs' subdirectory in the parent
            if dir.join("envs").exists() {
                return dir.to_path_buf();
            }
        }

        // Fallback to user home directory
        dirs::home_dir()
            .map(|h| h.join(".local/share/mamba"))
            .unwrap_or_else(|| PathBuf::from("/tmp/micromamba"))
    }

    /// Apply environment variables to a command
    fn apply_env_to_command(&self, cmd: &mut AsyncCommand) {
        for (key, value) in self.build_env_vars() {
            cmd.env(&key, &value);
        }
    }

    /// Find an explicitly installed micromamba executable without network access.
    pub fn find_micromamba() -> Result<PathBuf> {
        let explicitly_configured_path = std::env::var_os("ENVA_MICROMAMBA_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let path_discovery_result = which("micromamba").ok();
        resolve_micromamba_path(explicitly_configured_path, path_discovery_result)
    }

    /// Initialize environments from config directory
    async fn initialize_environments(&mut self, verbose: bool) -> Result<()> {
        // Auto-copy configuration templates if they don't exist
        self.auto_copy_config_templates(verbose).await?;

        // Load environment configurations
        for environment_name in BUILT_IN_ENV_NAMES {
            let environment_file = self.config_dir.join(format!("{}.yaml", environment_name));

            if !environment_file.exists() {
                if verbose {
                    warn!("Environment file not found: {:?}", environment_file);
                }
                continue;
            }

            // Get tools for this environment
            let tools = TOOL_ENVIRONMENT_MAP
                .iter()
                .filter_map(|(tool_name, mapped_environment_name)| {
                    if *mapped_environment_name == environment_name {
                        Some((*tool_name).to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>();

            // Check if environment exists
            let status = match self.environment_exists(environment_name).await {
                Ok(exists) => {
                    if exists {
                        EnvironmentStatus::Installed
                    } else {
                        EnvironmentStatus::NotInstalled
                    }
                }
                Err(error) => EnvironmentStatus::Error(error.to_string()),
            };

            let environment = MicromambaEnvironment {
                name: environment_name.to_string(),
                file_path: environment_file,
                tools,
                status,
                created_at: None,
            };

            self.environments
                .insert(environment_name.to_string(), environment);
        }

        if verbose {
            debug!("Initialized {} environments", self.environments.len());
        }
        Ok(())
    }

    /// Auto-copy configuration templates from source to target
    async fn auto_copy_config_templates(&self, verbose: bool) -> Result<()> {
        let source_config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("configs");

        let templates_are_present = BUILT_IN_ENV_NAMES.iter().all(|environment_name| {
            self.config_dir
                .join(format!("{}.yaml", environment_name))
                .exists()
        });
        if templates_are_present {
            if verbose {
                debug!("Configuration templates already exist, skipping copy");
            }
            return Ok(());
        }

        if verbose {
            info!("Auto-copying environment configuration templates...");
        }

        fs::create_dir_all(&self.config_dir).map_err(|error| {
            EnvError::FileOperation(format!("Failed to create config dir: {}", error))
        })?;

        for environment_name in BUILT_IN_ENV_NAMES {
            let file_name = format!("{}.yaml", environment_name);
            let source_file = source_config_dir.join(&file_name);
            let target_file = self.config_dir.join(&file_name);

            if source_file.exists() && !target_file.exists() {
                fs::copy(&source_file, &target_file).map_err(|error| {
                    EnvError::FileOperation(format!("Failed to copy {}: {}", file_name, error))
                })?;
                if verbose {
                    info!("✓ Copied configuration template: {}", file_name);
                }
            }
        }

        Ok(())
    }

    fn summary_spinner(message: impl Into<String>) -> Result<ProgressBar> {
        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .map_err(|e| EnvError::Template(format!("Failed to create progress bar: {}", e)))?
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
        pb.set_style(style);
        pb.set_message(message.into());
        pb.enable_steady_tick(Duration::from_millis(120));
        Ok(pb)
    }

    pub async fn clean_package_cache(&self, dry_run: bool, output_mode: OutputMode) -> Result<()> {
        if dry_run {
            info!("Dry-run: would clean package caches using {}", self.pm_type);
            return Ok(());
        }

        info!("Cleaning package caches using {}...", self.pm_type);
        let progress = if matches!(output_mode, OutputMode::Summary) {
            Some(Self::summary_spinner(format!(
                "Cleaning {} package caches...",
                self.pm_type
            ))?)
        } else {
            None
        };

        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("clean").arg("--all");

        match self.pm_type {
            PackageManager::Micromamba => {
                cmd.arg("--yes");
            }
            PackageManager::Conda | PackageManager::Mamba => {
                cmd.arg("-y");
            }
            PackageManager::None => {
                return Err(EnvError::Execution(
                    "No package manager available for cache cleaning".to_string(),
                ));
            }
        }

        let output = match output_mode {
            OutputMode::Stream => {
                if let Some(pb) = &progress {
                    pb.finish_and_clear();
                }
                cmd.stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                self.apply_env_to_command(&mut cmd);
                let status = cmd.status().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to clean package caches: {}", e))
                })?;
                std::process::Output {
                    status,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            }
            OutputMode::Summary | OutputMode::Quiet => {
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                self.apply_env_to_command(&mut cmd);
                let output = cmd.output().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to clean package caches: {}", e))
                })?;
                if let Some(pb) = &progress {
                    pb.finish_and_clear();
                }
                output
            }
        };

        if !output.status.success() {
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            return Err(EnvError::Execution(format!(
                "Failed to clean package caches: exit code {:?}",
                output.status.code()
            )));
        }

        if matches!(output_mode, OutputMode::Summary) {
            println!("✓ Cleaned {} package caches", self.pm_type);
        }

        info!("Package caches cleaned successfully");
        Ok(())
    }

    /// Create environment from YAML file
    pub async fn create_environment(
        &self,
        env_name: &str,
        yaml_file: &Path,
        dry_run: bool,
        force: bool,
        output_mode: OutputMode,
    ) -> Result<()> {
        let environment_name = EnvironmentName::parse(env_name.to_string())?;
        let env_name = environment_name.as_str();
        let _lock = self.creation_lock.lock().await;

        info!("create_environment called for {:?}", yaml_file);
        let progress = if matches!(output_mode, OutputMode::Summary) {
            Some(Self::summary_spinner(format!(
                "Preparing environment {}...",
                env_name
            ))?)
        } else {
            None
        };

        if dry_run {
            if let Some(pb) = &progress {
                pb.set_message(format!("Validating YAML for {}...", env_name));
            }
            let validation = self.validate_yaml(yaml_file).await?;
            if let Some(pb) = &progress {
                pb.finish_and_clear();
            }
            println!("{}", serde_json::to_string_pretty(&validation)?);
            return Ok(());
        }

        if self.environment_exists(env_name).await? {
            if force {
                if let Some(pb) = &progress {
                    pb.set_message(format!("Replacing existing environment {}...", env_name));
                }
                info!(
                    "Environment {} already exists; removing it before recreation",
                    env_name
                );
                self.remove_environment_with_output(env_name, output_mode)
                    .await?;
            } else {
                if let Some(pb) = &progress {
                    pb.finish_and_clear();
                }
                let error_msg = format!(
                    "Environment {} already exists. Re-run with --force to replace it.",
                    env_name
                );
                error!("{}", error_msg);
                return Err(EnvError::Execution(error_msg));
            }
        }

        if let Some(pb) = &progress {
            pb.set_message(format!("Validating YAML for {}...", env_name));
        }
        info!("About to validate YAML for {:?}", yaml_file);
        let _validation = self.validate_yaml(yaml_file).await?;
        info!("YAML validation complete");

        if let Some(pb) = &progress {
            pb.set_message(format!("Creating environment {}...", env_name));
        }
        info!("Building micromamba command for {:?}", yaml_file);
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env")
            .arg("create")
            .arg("-f")
            .arg(yaml_file)
            .arg("-n")
            .arg(env_name)
            .arg("-y");

        let output = match output_mode {
            OutputMode::Stream => {
                if let Some(pb) = &progress {
                    pb.finish_and_clear();
                }
                cmd.stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                self.apply_env_to_command(&mut cmd);
                info!("Command built, executing with streamed output...");
                let status = cmd.status().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to execute micromamba: {}", e))
                })?;
                std::process::Output {
                    status,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            }
            OutputMode::Summary | OutputMode::Quiet => {
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                self.apply_env_to_command(&mut cmd);
                info!("Command built, executing with captured output...");
                let output = cmd.output().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to execute micromamba: {}", e))
                })?;
                if let Some(pb) = &progress {
                    pb.finish_and_clear();
                }
                output
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }

            if stderr.contains("Non-conda folder exists at prefix") {
                let error_msg = "Failed to create environment: Environment directory already exists but is not a valid conda environment. Please remove the existing directory and try again, or use a different environment name.".to_string();
                error!("{}", error_msg);
                return Err(EnvError::Execution(error_msg));
            }

            let error_msg = format!(
                "Failed to create environment: micromamba command failed with exit code {:?}",
                output.status.code()
            );
            error!("{}", error_msg);
            return Err(EnvError::Execution(error_msg));
        }

        if matches!(output_mode, OutputMode::Summary) {
            println!("✓ Environment {} created", env_name);
        }

        self.invalidate_environment_list_cache();
        Self::invalidate_runtime_manager_cache(self.pm_type);
        info!("Environment created successfully from {:?}", yaml_file);
        Ok(())
    }

    async fn run_with_target_extended(
        &self,
        target_flag: &str,
        target: &str,
        command: &RunCommand,
        env_vars: &[String],
        cwd: &Path,
        capture_output: bool,
    ) -> Result<()> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("run").arg(target_flag).arg(target).arg("--");
        append_environment_run_command(&mut cmd, command)?;
        cmd.current_dir(cwd);

        self.apply_env_to_command(&mut cmd);
        for env_pair in env_vars {
            let (key, value) = env_pair.split_once('=').ok_or_else(|| {
                EnvError::Validation(format!("Invalid environment variable format: {}", env_pair))
            })?;
            cmd.env(key, value);
        }

        if capture_output {
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
        }

        let output =
            if capture_output {
                let output = cmd.output().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to execute command: {}", e))
                })?;

                if !output.stdout.is_empty() {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                }
                if !output.stderr.is_empty() {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }

                output
            } else {
                let status = cmd.status().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to execute command: {}", e))
                })?;
                std::process::Output {
                    status,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            };

        if !output.status.success() {
            return Err(EnvError::ProcessExit {
                code: output.status.code(),
            });
        }

        Ok(())
    }

    /// Run command in environment
    pub async fn run_in_environment(&self, env_name: &str, command: &str) -> Result<Output> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("run").arg("-n").arg(env_name).arg("--");
        append_environment_shell_arguments(&mut cmd, command);

        self.apply_env_to_command(&mut cmd);

        cmd.output().await.map_err(|e| {
            EnvError::Execution(format!("Failed to run command in environment: {}", e))
        })
    }

    /// Run command in environment with extended options
    pub async fn run_in_environment_extended(
        &self,
        env_name: &str,
        command: &RunCommand,
        env_vars: &[String],
        cwd: &Path,
        capture_output: bool,
    ) -> Result<()> {
        self.run_with_target_extended("-n", env_name, command, env_vars, cwd, capture_output)
            .await
    }

    /// Run command using an explicit environment prefix
    pub async fn run_in_environment_by_prefix_extended(
        &self,
        prefix: &Path,
        command: &RunCommand,
        env_vars: &[String],
        cwd: &Path,
        capture_output: bool,
    ) -> Result<()> {
        let prefix = prefix.to_string_lossy().to_string();
        self.run_with_target_extended("-p", &prefix, command, env_vars, cwd, capture_output)
            .await
    }

    fn invalidate_environment_list_cache(&self) {
        if let Ok(mut cache) = self.env_list_cache.lock() {
            *cache = None;
        }
    }

    /// List environment prefixes visible to the configured package manager
    pub async fn list_environment_prefixes(&self) -> Result<Vec<PathBuf>> {
        if let Ok(cache) = self.env_list_cache.lock() {
            if let Some(envs) = cache.as_ref() {
                return Ok(envs.clone());
            }
        }

        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env").arg("list").arg("--json");

        self.apply_env_to_command(&mut cmd);

        let output = cmd
            .output()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to list environments: {}", e)))?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::Execution(format!(
                "Failed to list environments: {}",
                error
            )));
        }

        #[derive(Deserialize)]
        struct EnvList {
            envs: Vec<String>,
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let envs: EnvList = serde_json::from_str(&json_str).map_err(|e| {
            EnvError::Validation(format!("Failed to parse environment list: {}", e))
        })?;
        let envs: Vec<PathBuf> = envs.envs.into_iter().map(PathBuf::from).collect();

        if let Ok(mut cache) = self.env_list_cache.lock() {
            *cache = Some(envs.clone());
        }

        Ok(envs)
    }

    fn get_base_environment_prefix(&self) -> PathBuf {
        match self.pm_type {
            PackageManager::Micromamba => self.get_mamba_root_prefix(),
            PackageManager::Conda | PackageManager::Mamba => self
                .pm_path
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .or_else(|| std::env::var("CONDA_PREFIX").ok().map(PathBuf::from))
                .unwrap_or_else(|| self.get_mamba_root_prefix()),
            PackageManager::None => self.get_mamba_root_prefix(),
        }
    }

    fn environment_name_matches(&self, env: &Path, env_name: &str) -> bool {
        env.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == env_name)
            .unwrap_or(false)
            || (env_name == "base" && env == self.get_base_environment_prefix())
    }

    /// Find all matching environment prefixes for a given environment name
    pub async fn find_environment_prefixes(&self, env_name: &str) -> Result<Vec<PathBuf>> {
        Ok(self
            .list_environment_prefixes()
            .await?
            .into_iter()
            .filter(|env| self.environment_name_matches(env, env_name))
            .collect())
    }

    /// Check if environment exists
    pub async fn environment_exists(&self, env_name: &str) -> Result<bool> {
        Ok(!self.find_environment_prefixes(env_name).await?.is_empty())
    }

    /// Remove environment
    pub async fn remove_environment(&self, env_name: &str) -> Result<()> {
        self.remove_environment_with_output(env_name, OutputMode::Stream)
            .await
    }

    /// Remove environment with configurable output handling
    async fn remove_environment_target_with_output(
        &self,
        target_flag: &str,
        target: &str,
        display_name: &str,
        output_mode: OutputMode,
    ) -> Result<()> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env")
            .arg("remove")
            .arg(target_flag)
            .arg(target)
            .arg("-y");

        let stashed_ownership_marker = if target_flag == "-p" {
            Self::stash_ownership_marker(Path::new(target))?
        } else {
            None
        };

        let output_result = match output_mode {
            OutputMode::Stream => {
                cmd.stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                self.apply_env_to_command(&mut cmd);
                cmd.status()
                    .await
                    .map(|status| std::process::Output {
                        status,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    })
                    .map_err(|e| {
                        EnvError::Execution(format!("Failed to remove environment: {}", e))
                    })
            }
            OutputMode::Summary | OutputMode::Quiet => {
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                self.apply_env_to_command(&mut cmd);
                cmd.output().await.map_err(|e| {
                    EnvError::Execution(format!("Failed to remove environment: {}", e))
                })
            }
        };
        let restore_result = if target_flag == "-p" {
            Self::restore_ownership_marker(Path::new(target), stashed_ownership_marker)
        } else {
            Ok(())
        };

        let output = match (output_result, restore_result) {
            (Ok(output), Ok(())) => output,
            (Err(error), Ok(())) => return Err(error),
            (Ok(output), Err(error)) if output.status.success() => return Err(error),
            (Ok(output), Err(error)) => {
                return Err(EnvError::Execution(format!(
                    "Failed to remove environment: exit code {:?}; additionally failed to restore enva ownership marker in {}: {}",
                    output.status.code(),
                    target,
                    error
                )));
            }
            (Err(remove_error), Err(restore_error)) => {
                return Err(EnvError::Execution(format!(
                    "{}; additionally failed to restore enva ownership marker in {}: {}",
                    remove_error, target, restore_error
                )));
            }
        };

        if !output.status.success() {
            if !output.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            return Err(EnvError::Execution(format!(
                "Failed to remove environment: exit code {:?}",
                output.status.code()
            )));
        }

        if matches!(output_mode, OutputMode::Summary) {
            println!("✓ Removed environment {}", display_name);
        }

        self.invalidate_environment_list_cache();
        Self::invalidate_runtime_manager_cache(self.pm_type);
        info!("Environment {} removed successfully", display_name);
        Ok(())
    }

    pub async fn remove_environment_with_output(
        &self,
        env_name: &str,
        output_mode: OutputMode,
    ) -> Result<()> {
        let environment_name = EnvironmentName::parse(env_name.to_string())?;
        self.remove_environment_target_with_output(
            "-n",
            environment_name.as_str(),
            environment_name.as_str(),
            output_mode,
        )
        .await
    }

    pub async fn remove_environment_by_prefix_with_output(
        &self,
        prefix: &Path,
        output_mode: OutputMode,
    ) -> Result<()> {
        let prefix_str = prefix.to_string_lossy().to_string();
        self.remove_environment_target_with_output("-p", &prefix_str, &prefix_str, output_mode)
            .await
    }

    /// Validate YAML file (dry-run)
    pub async fn validate_yaml(&self, yaml_file: &Path) -> Result<ValidationResult> {
        // Read and parse YAML
        let content = fs::read_to_string(yaml_file)
            .map_err(|e| EnvError::FileOperation(format!("Failed to read YAML file: {}", e)))?;

        let config: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| EnvError::Validation(format!("Invalid YAML syntax: {}", e)))?;

        // Extract environment name
        let env_name = config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Validate syntax (basic check)
        let syntax_valid = config.get("dependencies").is_some();

        // TODO: Add more sophisticated validation
        // - Check dependencies are resolvable
        // - Check version conflicts
        // - Check channels are accessible

        let validation = ValidationDetails {
            syntax_valid,
            dependencies_resolvable: true, // Placeholder
            version_conflicts: vec![],     // Placeholder
            channels_accessible: true,     // Placeholder
        };

        // Estimate package count and size
        let estimated_packages = config
            .get("dependencies")
            .and_then(|d| d.as_sequence())
            .map(|seq| seq.len())
            .unwrap_or(0);

        let estimated_size_mb = (estimated_packages as u64) * 10; // Rough estimate: 10MB per package

        Ok(ValidationResult {
            dry_run: true,
            environment: env_name,
            yaml_file: yaml_file.to_path_buf(),
            validation,
            estimated_packages,
            estimated_size_mb,
            channels_accessible: vec![],
        })
    }

    /// List all environments with status
    pub async fn list_environments(&self) -> Result<Vec<MicromambaEnvironment>> {
        let mut envs = Vec::new();

        for (name, env) in &self.environments {
            // Check if environment still exists
            let status = match self.environment_exists(name).await {
                Ok(exists) => {
                    if exists {
                        EnvironmentStatus::Ready
                    } else {
                        EnvironmentStatus::NotInstalled
                    }
                }
                Err(e) => EnvironmentStatus::Error(e.to_string()),
            };

            let mut env_clone = env.clone();
            env_clone.status = status;
            envs.push(env_clone);
        }

        Ok(envs)
    }

    /// Get package manager executable path
    /// Note: Method name kept for backward compatibility
    pub fn micromamba_path(&self) -> &PathBuf {
        &self.pm_path
    }

    /// Get environment by name
    pub fn get_environment(&self, name: &str) -> Option<&MicromambaEnvironment> {
        self.environments.get(name)
    }

    /// Get all environments
    pub fn get_all_environments(&self) -> &HashMap<String, MicromambaEnvironment> {
        &self.environments
    }

    /// Get environment statuses (for compatibility with CondaManager API)
    pub fn get_environment_statuses(&self) -> &HashMap<String, MicromambaEnvironment> {
        &self.environments
    }

    /// Install packages in environment
    pub async fn install_packages(
        &self,
        env_name: &str,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()> {
        if !self.environment_exists(env_name).await? {
            return Err(EnvError::Execution(format!(
                "Environment '{}' does not exist. Please create it first using 'xdxtools env create --name {}'",
                env_name,
                env_name
            )));
        }

        let prefix = self
            .find_environment_prefixes(env_name)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                EnvError::Execution(format!(
                    "Environment '{}' does not have a resolvable prefix",
                    env_name
                ))
            })?;

        self.install_packages_by_prefix(&prefix, packages, output_mode)
            .await
    }

    pub async fn install_packages_by_prefix(
        &self,
        prefix: &Path,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()> {
        use tokio::process::Command as AsyncCommand;

        let _lock = self.creation_lock.lock().await;

        if packages.is_empty() {
            return Ok(());
        }

        if !prefix.join("conda-meta").is_dir() {
            return Err(EnvError::Execution(format!(
                "Environment prefix is not a valid conda environment: {}",
                prefix.display()
            )));
        }

        info!(
            "Installing packages in environment prefix '{}': {:?}",
            prefix.display(),
            packages
        );
        debug!("package manager path: {:?}", self.pm_path);

        let progress = if matches!(output_mode, OutputMode::Summary) {
            Some(Self::summary_spinner(format!(
                "Installing packages into {} via {}...",
                prefix.display(),
                self.pm_type
            ))?)
        } else {
            None
        };

        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("install")
            .arg("-p")
            .arg(prefix)
            .arg("--override-channels")
            .arg("-c")
            .arg("conda-forge")
            .arg("-c")
            .arg("bioconda")
            .arg("-y");

        for package in packages {
            cmd.arg(package);
        }

        let stashed_ownership_marker = Self::stash_ownership_marker(prefix)?;
        let output_result = match output_mode {
            OutputMode::Stream => {
                println!(
                    "Installing packages into {} via {}...",
                    prefix.display(),
                    self.pm_type
                );
                cmd.stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                self.apply_env_to_command(&mut cmd);
                cmd.status()
                    .await
                    .map(|status| std::process::Output {
                        status,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    })
                    .map_err(|e| {
                        EnvError::Execution(format!(
                            "Failed to execute {} install: {}",
                            self.pm_type, e
                        ))
                    })
            }
            OutputMode::Summary | OutputMode::Quiet => {
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                self.apply_env_to_command(&mut cmd);
                cmd.output().await.map_err(|e| {
                    EnvError::Execution(format!(
                        "Failed to execute {} install: {}",
                        self.pm_type, e
                    ))
                })
            }
        };

        let restore_result = Self::restore_ownership_marker(prefix, stashed_ownership_marker);

        let result = match (output_result, restore_result) {
            (Ok(output), Ok(())) if output.status.success() => Ok(output),
            (Ok(output), Ok(())) => Err(EnvError::Execution(format!(
                "Failed to install packages into {} using {}: exit code {:?}{}",
                prefix.display(),
                self.pm_type,
                output.status.code(),
                captured_command_failure_detail(&output)
            ))),
            (Err(error), Ok(())) => Err(error),
            (Ok(output), Err(error)) if output.status.success() => Err(error),
            (Ok(output), Err(error)) => Err(EnvError::Execution(format!(
                "Failed to install packages into {} using {}: exit code {:?}{}; additionally failed to restore enva ownership marker: {}",
                prefix.display(),
                self.pm_type,
                output.status.code(),
                captured_command_failure_detail(&output),
                error
            ))),
            (Err(install_error), Err(restore_error)) => Err(EnvError::Execution(format!(
                "{}; additionally failed to restore enva ownership marker in {}: {}",
                install_error,
                prefix.display(),
                restore_error
            ))),
        };

        match (&progress, &result) {
            (Some(pb), Ok(_)) => pb.finish_and_clear(),
            (Some(pb), Err(error)) => pb.abandon_with_message(format!(
                "✗ Failed package install for {}: {}",
                prefix.display(),
                error
            )),
            _ => {}
        }

        match result {
            Ok(output) => {
                if matches!(output_mode, OutputMode::Summary) {
                    println!("✓ Installed packages into {}", prefix.display());
                }
                if !output.stdout.is_empty() && matches!(output_mode, OutputMode::Stream) {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                }
                if !output.stderr.is_empty() && matches!(output_mode, OutputMode::Stream) {
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
                info!(
                    "Successfully installed packages in environment prefix '{}'",
                    prefix.display()
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Update environment statuses (for compatibility with CondaManager API)
    pub async fn update_environment_statuses(&mut self) -> Result<()> {
        let environment_names: Vec<String> = self.environments.keys().cloned().collect();

        for env_name in environment_names {
            if self.environment_exists(&env_name).await? {
                if let Some(environment) = self.environments.get_mut(&env_name) {
                    if environment.status != crate::micromamba::EnvironmentStatus::Ready {
                        environment.status = crate::micromamba::EnvironmentStatus::Ready;
                        info!("Environment '{}' is ready", env_name);
                    }
                }
            } else {
                if let Some(environment) = self.environments.get_mut(&env_name) {
                    if environment.status == crate::micromamba::EnvironmentStatus::Ready {
                        environment.status = crate::micromamba::EnvironmentStatus::NotInstalled;
                        warn!("Environment '{}' is not installed", env_name);
                    }
                }
            }
        }

        Ok(())
    }

    /// Generate environment file content (for compatibility with CondaManager API)
    pub fn generate_environment_file(&self, env_name: &str) -> Result<String> {
        match env_name {
            CORE_ENV_NAME => Ok(self.generate_otter_core_yaml()),
            SNAKEMAKE_ENV_NAME => Ok(self.generate_otter_snakemake_yaml()),
            EXTRA_ENV_NAME => Ok(self.generate_otter_extra_yaml()),
            _ => Err(EnvError::Validation(format!(
                "Unknown environment: {}",
                env_name
            ))),
        }
    }

    /// Generate otter-core environment YAML content
    fn generate_otter_core_yaml(&self) -> String {
        r#"name: otter-core
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.8.18
  - numpy=1.24.4
  - pandas
  - fastqc
  - multiqc=1.19
  - seqkit
  - seqtk
  - qualimap
  - bismark
  - trim-galore
  - samtools=1.15.1
  - hdf5
  - star
  - htseq=2.0.3
  - rmats=4.1.2
  - picard
  - macs2=2.2.7.1
  - bwa=0.7.17
  - bowtie2=2.5.4
  - pyyaml
"#
        .to_string()
    }

    /// Generate otter-snakemake environment YAML content
    fn generate_otter_snakemake_yaml(&self) -> String {
        r#"name: otter-snakemake
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.11.9
  - numpy=1.24.4
  - snakemake=9.4.1
  - pyyaml
  - pandas
  - matplotlib
  - networkx
  - graphviz
  - jinja2
  - click
  - requests
  - packaging
  - git
  - gitpython
"#
        .to_string()
    }

    /// Generate otter-extra environment YAML content
    fn generate_otter_extra_yaml(&self) -> String {
        r#"name: otter-extra
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.10.13
  - numpy=1.24.4
  - pandas
  - bedtools
  - bcftools
  - vcftools
  - tabix
  - scipy
  - scikit-learn
  - statsmodels
  - plotly
  - dash
  - streamlit
  - jupyter
  - jupyterlab
  - flask
  - requests
  - beautifulsoup4
  - openpyxl
  - xlsxwriter
  - go
  - rust
  - deepTools=3.5.5
  - genrich=0.6
  - homer=4.11
"#
        .to_string()
    }

    /// Get all conda environments from the system
    ///
    /// This function executes `conda env list` (or mamba/micromamba) and parses the output
    /// to return a list of all conda environments with their names and prefixes.
    pub async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>> {
        if self.pm_type == PackageManager::None {
            return Ok(vec![]);
        }

        debug!("Executing {} env list", self.pm_type);

        let active_prefix = std::env::var("CONDA_PREFIX").ok();
        let base_prefix = self.get_base_environment_prefix();
        let source = self.pm_type.to_string();

        Ok(self
            .list_environment_prefixes()
            .await?
            .into_iter()
            .map(|prefix| {
                let name = if prefix == base_prefix {
                    "base".to_string()
                } else {
                    prefix
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                };

                CondaEnvironment {
                    name,
                    prefix: prefix.display().to_string(),
                    is_active: active_prefix
                        .as_deref()
                        .map(|active| Path::new(active) == prefix)
                        .unwrap_or(false),
                    source: Some(source.clone()),
                    owner: None,
                    adopted_from: None,
                }
            })
            .collect())
    }
}

/// Conda environment information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CondaEnvironment {
    /// Environment name
    pub name: String,
    /// Environment prefix path
    pub prefix: String,
    /// Whether this environment is currently active
    pub is_active: bool,
    /// Source backend or package manager that reported this environment
    pub source: Option<String>,
    /// Ownership authority for this environment
    pub owner: Option<String>,
    /// Original package manager source when a foreign environment was adopted by rattler
    pub adopted_from: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    fn create_fake_environment(prefix: &Path) {
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(
            prefix.join("conda-meta").join("history"),
            "created by test\n",
        )
        .unwrap();
    }

    fn build_test_manager(config_dir: &Path) -> MicromambaManager {
        MicromambaManager {
            pm_path: PathBuf::from("micromamba"),
            pm_type: PackageManager::Micromamba,
            environments: HashMap::new(),
            config_dir: config_dir.to_path_buf(),
            version_config: VersionConfig::default(),
            creation_lock: Arc::new(Mutex::new(())),
            env_list_cache: Arc::new(StdMutex::new(None)),
        }
    }

    #[test]
    fn captured_failure_detail_prefers_stderr_and_truncates_tail() {
        let mut stderr: Vec<u8> = vec![b'x'; CAPTURED_FAILURE_OUTPUT_LIMIT_BYTES + 8];
        stderr.extend_from_slice(b"solver failure");
        let output: Output = Output {
            status: std::process::Command::new("false").status().unwrap(),
            stdout: b"less useful stdout".to_vec(),
            stderr,
        };

        let detail: String = captured_command_failure_detail(&output);
        assert!(detail.contains("stderr"));
        assert!(detail.contains("truncated"));
        assert!(detail.contains("solver failure"));
        assert!(!detail.contains("less useful stdout"));
    }

    #[test]
    fn captured_failure_detail_falls_back_to_stdout() {
        let output: Output = Output {
            status: std::process::Command::new("false").status().unwrap(),
            stdout: b"solver wrote to stdout".to_vec(),
            stderr: Vec::new(),
        };

        assert_eq!(
            captured_command_failure_detail(&output),
            "; stdout: solver wrote to stdout"
        );
    }

    #[tokio::test]
    async fn test_validate_yaml() {
        let temp_dir = tempdir().unwrap();
        let yaml_file = temp_dir.path().join("test.yaml");

        let yaml_content = r#"
name: test-env
channels:
  - conda-forge
dependencies:
  - python=3.10
  - numpy
"#;
        fs::write(&yaml_file, yaml_content).unwrap();

        let manager = build_test_manager(temp_dir.path());
        let result = manager.validate_yaml(&yaml_file).await.unwrap();

        assert!(result.validation.syntax_valid);
        assert_eq!(result.environment, "test-env");
        assert!(result.estimated_packages >= 2);
    }

    #[tokio::test]
    async fn test_environment_list() {
        let temp_dir = tempdir().unwrap();
        let mut manager = build_test_manager(temp_dir.path());

        manager.initialize_environments(false).await.unwrap();
        let envs = manager.list_environments().await.unwrap();

        assert_eq!(envs.len(), 3);
        assert!(envs.iter().any(|env| env.name == CORE_ENV_NAME));
        assert!(envs.iter().any(|env| env.name == SNAKEMAKE_ENV_NAME));
        assert!(envs.iter().any(|env| env.name == EXTRA_ENV_NAME));
    }

    #[test]
    fn ownership_marker_can_be_stashed_and_restored() {
        let temp_dir = tempdir().unwrap();
        let prefix = temp_dir.path().join("envs").join("demo");
        create_fake_environment(&prefix);
        fs::write(
            prefix.join("conda-meta").join("enva-rattler.json"),
            r#"{"version":1,"owner":"rattler"}"#,
        )
        .unwrap();

        let stashed = MicromambaManager::stash_ownership_marker(&prefix).unwrap();
        assert_eq!(
            stashed.as_deref(),
            Some(r#"{"version":1,"owner":"rattler"}"#)
        );
        assert!(!prefix.join("conda-meta").join("enva-rattler.json").exists());

        MicromambaManager::restore_ownership_marker(&prefix, stashed).unwrap();
        assert_eq!(
            fs::read_to_string(prefix.join("conda-meta").join("enva-rattler.json")).unwrap(),
            r#"{"version":1,"owner":"rattler"}"#
        );
    }

    #[test]
    fn ownership_marker_restore_is_noop_when_absent() {
        let temp_dir = tempdir().unwrap();
        let prefix = temp_dir.path().join("envs").join("demo");
        create_fake_environment(&prefix);

        let stashed = MicromambaManager::stash_ownership_marker(&prefix).unwrap();
        assert!(stashed.is_none());

        MicromambaManager::restore_ownership_marker(&prefix, stashed).unwrap();
        assert!(!prefix.join("conda-meta").join("enva-rattler.json").exists());
    }

    #[cfg(unix)]
    fn create_fake_micromamba(binary_path: &Path, version: &str) {
        fs::write(
            binary_path,
            format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n"),
        )
        .unwrap();
        let mut permissions = fs::metadata(binary_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(binary_path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicitly_configured_micromamba_path_takes_precedence() {
        let temporary_directory = tempdir().unwrap();
        let configured_binary = temporary_directory.path().join("configured-micromamba");
        let path_binary = temporary_directory.path().join("path-micromamba");
        create_fake_micromamba(&configured_binary, "configured");
        create_fake_micromamba(&path_binary, "path");

        let resolved =
            resolve_micromamba_path(Some(configured_binary.clone()), Some(path_binary)).unwrap();

        assert_eq!(resolved, configured_binary.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn invalid_explicit_path_does_not_fall_back_to_path_discovery() {
        let temporary_directory = tempdir().unwrap();
        let missing_binary = temporary_directory.path().join("missing-micromamba");
        let path_binary = temporary_directory.path().join("path-micromamba");
        create_fake_micromamba(&path_binary, "path");

        let error = resolve_micromamba_path(Some(missing_binary), Some(path_binary)).unwrap_err();

        assert!(error.to_string().contains("ENVA_MICROMAMBA_PATH"));
    }

    #[cfg(unix)]
    #[test]
    fn path_discovery_accepts_an_existing_healthy_executable() {
        let temporary_directory = tempdir().unwrap();
        let path_binary = temporary_directory.path().join("micromamba");
        create_fake_micromamba(&path_binary, "2.8.1");

        let resolved = resolve_micromamba_path(None, Some(path_binary.clone())).unwrap();

        assert_eq!(resolved, path_binary.canonicalize().unwrap());
    }

    #[test]
    fn missing_micromamba_returns_manual_installation_guidance() {
        let error = resolve_micromamba_path(None, None).unwrap_err();
        let message = error.to_string();

        assert!(message.contains("optional compatibility backend"));
        assert!(message.contains("ENVA_MICROMAMBA_PATH"));
    }
}
