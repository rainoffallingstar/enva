//! Micromamba environment management for enva
//!
//! Provides simplified micromamba environment management for the 4 core environments:
//! - xdxtools-core: All bioinformatics tools (without qualimap)
//! - xdxtools-r: R/Bioconductor packages + qualimap
//! - xdxtools-snakemake: Workflow engine
//! - xdxtools-extra: Additional visualization and analysis tools (without R packages)
//!
//! Key features:
//! - Automatic micromamba installation if not found
//! - Dry-run validation for YAML files
//! - JSON output for automation
//! - Cross-platform support (Linux/macOS)
//! - Performance: 2-3x faster than conda

use crate::error::{Result, EnvError};
use crate::package_manager::{PackageManager, PackageManagerDetector};
use async_trait::async_trait;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::process::Command as AsyncCommand;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use which::which;

/// Global micromamba manager instance (lazy initialization)
static GLOBAL_MANAGER: LazyLock<Mutex<Option<MicromambaManager>>> = LazyLock::new(|| Mutex::new(None));

/// Track whether global manager has been initialized (to control logging)
static GLOBAL_INITIALIZED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

/// Tool to environment mapping (updated for micromamba)
pub const TOOL_ENVIRONMENT_MAP: &[(&str, &str)] = &[
    // QC Tools -> xdxtools-core
    ("fastqc", "xdxtools-core"),
    ("multiqc", "xdxtools-core"),
    ("seqkit", "xdxtools-core"),
    ("seqtk", "xdxtools-core"),
    ("samtools", "xdxtools-core"),
    ("picard", "xdxtools-core"),
    // Methylation Tools -> xdxtools-core
    ("bismark", "xdxtools-core"),
    ("trim_galore", "xdxtools-core"),
    ("trim-galore", "xdxtools-core"),
    // RNA-seq Tools -> xdxtools-core
    ("star", "xdxtools-core"),
    ("htseq-count", "xdxtools-core"),
    ("htseq", "xdxtools-core"),
    ("rmats", "xdxtools-core"),
    // ChIP-seq/ATAC-seq Core Tools -> xdxtools-core
    ("macs2", "xdxtools-core"),
    ("bwa", "xdxtools-core"),
    ("bowtie2", "xdxtools-core"),
    ("phantompeakqualtools", "xdxtools-core"),
    ("bwa-index", "xdxtools-core"),     // BWA index building
    ("bowtie2-build", "xdxtools-core"), // Bowtie2 index building
    // Qualimap -> xdxtools-r (moved from core)
    ("qualimap", "xdxtools-r"),
    // R packages -> xdxtools-r
    ("R", "xdxtools-r"),
    ("Rscript", "xdxtools-r"),
    // Snakemake -> xdxtools-snakemake
    ("snakemake", "xdxtools-snakemake"),
    ("jinja2", "xdxtools-snakemake"),
    ("click", "xdxtools-snakemake"),
    ("git", "xdxtools-snakemake"),
    // Advanced Bioinformatics -> xdxtools-extra
    ("bedtools", "xdxtools-extra"),
    ("bcftools", "xdxtools-extra"),
    ("vcftools", "xdxtools-extra"),
    ("tabix", "xdxtools-extra"),
    // Advanced ChIP-seq/ATAC-seq Tools -> xdxtools-extra
    ("deepTools", "xdxtools-extra"),
    ("genrich", "xdxtools-extra"),
    ("homer", "xdxtools-extra"),
    // Data Science & Visualization -> xdxtools-extra (Python only)
    ("jupyter", "xdxtools-extra"),
    ("jupyterlab", "xdxtools-extra"),
    ("flask", "xdxtools-extra"),
    ("dash", "xdxtools-extra"),
    ("streamlit", "xdxtools-extra"),
    ("scikit-learn", "xdxtools-extra"),
    ("scipy", "xdxtools-extra"),
    ("statsmodels", "xdxtools-extra"),
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
    /// Detects and uses: conda → mamba → micromamba (in priority order)
    pub async fn new() -> Result<Self> {
        // 1. Detect package manager with priority
        let mut detector = PackageManagerDetector::new();
        let pm_type = detector.detect_with_env_override()?;

        // 2. Get PM path based on detection
        let pm_path = match pm_type {
            PackageManager::Conda | PackageManager::Mamba => {
                // conda/mamba usually in PATH
                which(pm_type.command()).map_err(|_| {
                    EnvError::Config(format!("{} not found in PATH", pm_type))
                })?
            }
            PackageManager::Micromamba => {
                // micromamba might need to be downloaded
                Self::find_or_install_micromamba().await?
            }
            PackageManager::None => {
                return Err(EnvError::Config(
                    "No package manager found (conda/mamba/micromamba)".to_string()
                ));
            }
        };

        info!("✓ Package manager: {} ({})", pm_type, pm_path.display());

        // 3. Initialize other fields
        let config_dir = Self::get_cache_config_dir()?;

        let mut manager = Self {
            pm_path,
            pm_type,
            environments: HashMap::new(),
            config_dir,
            version_config: VersionConfig::default(),
            creation_lock: Arc::new(Mutex::new(())),
        };

        manager.initialize_environments(true).await?;
        Ok(manager)
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
        let pm_path = Self::find_or_install_micromamba().await?;
        let config_dir = config_dir.as_ref().to_path_buf();

        let mut manager = Self {
            pm_path,
            pm_type: PackageManager::Micromamba,
            environments: HashMap::new(),
            config_dir,
            version_config: VersionConfig::default(),
            creation_lock: Arc::new(Mutex::new(())),
        };

        manager.initialize_environments(true).await?;
        Ok(manager)
    }

    /// Create micromamba manager with custom version configuration
    pub async fn with_version_config<P: AsRef<Path>>(
        config_dir: P,
        version_config: VersionConfig,
    ) -> Result<Self> {
        let pm_path = Self::find_or_install_micromamba().await?;
        let config_dir = config_dir.as_ref().to_path_buf();

        let mut manager = Self {
            pm_path,
            pm_type: PackageManager::Micromamba,
            environments: HashMap::new(),
            config_dir,
            version_config,
            creation_lock: Arc::new(Mutex::new(())),
        };

        manager.initialize_environments(true).await?;
        Ok(manager)
    }

    /// Get the cache directory path (for logging/debugging)
    pub fn get_cache_dir(&self) -> &PathBuf {
        &self.config_dir
    }

    /// Build environment variables for micromamba subprocess execution
    fn build_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        // Get package manager directory
        let pm_dir = self
            .pm_path
            .parent()
            .unwrap_or(Path::new("."));

        // Calculate MAMBA_ROOT_PREFIX (parent of micromamba binary or dedicated prefix)
        let mamba_root = self.get_mamba_root_prefix();

        // Set LD_LIBRARY_PATH
        let lib_dir = pm_dir.join("lib");
        let existing_ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        let new_ld_path = if existing_ld_path.is_empty() {
            lib_dir.to_string_lossy().to_string()
        } else {
            format!(
                "{}:{}",
                lib_dir.to_string_lossy(),
                existing_ld_path
            )
        };
        env_vars.insert("LD_LIBRARY_PATH".to_string(), new_ld_path);

        // Set PATH
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!(
            "{}:{}",
            pm_dir.to_string_lossy(),
            existing_path
        );
        env_vars.insert("PATH".to_string(), new_path);

        // Note: Don't set MAMBA_ROOT_PREFIX to avoid overriding micromamba's default configuration
        // Let micromamba use its default base environment location

        env_vars
    }

    /// Get MAMBA_ROOT_PREFIX path
    fn get_mamba_root_prefix(&self) -> PathBuf {
        // First check if environment variable is already set
        if let Ok(prefix) = std::env::var("MAMBA_ROOT_PREFIX") {
            return PathBuf::from(prefix);
        }

        // Default to parent directory of micromamba binary
        self.pm_path
            .parent()
            .and_then(|dir| {
                // Check if this looks like a standard micromamba installation
                // Standard path: /path/to/bin/micromamba with root at /path/to/share/mamba
                let potential_root = dir.parent().map(|p| p.join("share/mamba"));
                if let Some(root) = potential_root {
                    if root.exists() {
                        return Some(root);
                    }
                }
                // Fallback to parent directory
                Some(dir.to_path_buf())
            })
            .unwrap_or_else(|| {
                // Fallback to user home directory
                dirs::home_dir()
                    .map(|h| h.join(".local/share/mamba"))
                    .unwrap_or_else(|| PathBuf::from("/tmp/micromamba"))
            })
    }

    /// Apply environment variables to a command
    fn apply_env_to_command(&self, cmd: &mut AsyncCommand) {
        for (key, value) in self.build_env_vars() {
            cmd.env(&key, &value);
        }
    }

    /// Determine installation directory for micromamba
    fn determine_install_directory() -> Result<PathBuf> {
        // Priority order:
        // 1. MICROMAMBA_INSTALL_DIR environment variable
        // 2. XDG_DATA_HOME/micromamba
        // 3. ~/.local/share/micromamba
        // 4. Relative to executable directory

        if let Ok(custom_dir) = std::env::var("MICROMAMBA_INSTALL_DIR") {
            return Ok(PathBuf::from(custom_dir).join("micromamba"));
        }

        if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
            return Ok(PathBuf::from(data_home).join("micromamba").join("micromamba"));
        }

        if let Some(home) = dirs::home_dir() {
            return Ok(home.join(".local").join("share").join("micromamba").join("micromamba"));
        }

        // Fallback to executable directory
        if let Ok(current_exe) = std::env::current_exe() {
            if let Some(exe_dir) = current_exe.parent() {
                let canonical_dir = exe_dir.canonicalize().map_err(|e| {
                    EnvError::FileOperation(format!("Failed to canonicalize exe dir: {}", e))
                })?;
                return Ok(canonical_dir.join("micromamba"));
            }
        }

        Err(EnvError::FileOperation(
            "Could not determine installation directory for micromamba".to_string()
        ))
    }

    /// Find or install micromamba executable
    pub async fn find_or_install_micromamba() -> Result<PathBuf> {
        // First, check if micromamba is already in PATH
        if let Ok(path) = which("micromamba") {
            info!("Found micromamba in PATH: {:?}", path);
            return normalize_and_validate_path(&path);
        }

        // Check common installation locations
        let common_paths = vec![
            "/usr/local/bin/micromamba",
            "/opt/micromamba/bin/micromamba",
            "/usr/bin/micromamba",
        ];

        for path in &common_paths {
            let path_buf = PathBuf::from(path);
            if path_buf.exists() {
                info!("Found micromamba at common location: {}", path);
                return normalize_and_validate_path(&path_buf);
            }
        }

        // If not found, install it
        info!("Micromamba not found, installing...");
        let install_path = Self::install_micromamba().await?;
        info!("Micromamba installed successfully at: {:?}", install_path);

        Ok(install_path)
    }

    /// Install micromamba automatically
    async fn install_micromamba() -> Result<PathBuf> {
        // Determine target architecture
        let arch = std::env::consts::ARCH;
        let target = match arch {
            "x86_64" => "x64",
            "aarch64" => "aarch64",
            _ => {
                return Err(EnvError::Validation(format!(
                    "Unsupported architecture: {}. Supported: x86_64, aarch64",
                    arch
                )));
            }
        };

        // Determine OS
        let os = std::env::consts::OS;
        let platform = match os {
            "linux" => "linux-64",
            "macos" => {
                // Check if Intel or Apple Silicon
                if arch == "aarch64" {
                    "osx-arm64"
                } else {
                    "osx-64"
                }
            }
            "windows" => "win-64",
            _ => {
                return Err(EnvError::Validation(format!(
                    "Unsupported OS: {}. Supported: linux, macos, windows",
                    os
                )));
            }
        };

        // Build download URLs (try multiple sources)
        let primary_url = format!("https://micro.mamba.pm/api/micromamba/{}/latest", platform);
        let github_url = format!(
            "https://github.com/mamba-org/micromamba-releases/releases/latest/download/micromamba-{}-{}",
            match os {
                "linux" => "linux",
                "macos" => "osx",
                "windows" => "win",
                _ => "unknown",
            },
            match arch {
                "x86_64" => "64",
                "aarch64" => "arm64",
                _ => "unknown",
            }
        );

        let download_urls = vec![
            ("Official Micromamba", primary_url),
            ("GitHub Releases", github_url),
        ];

        // Determine installation directory
        let install_dir = Self::determine_install_directory()?;

        // Ensure install_dir is absolute
        let install_dir = if install_dir.is_absolute() {
            install_dir
        } else {
            std::env::current_dir()
                .map_err(|e| {
                    EnvError::FileOperation(format!("Failed to get current directory: {}", e))
                })?
                .join(&install_dir)
        };

        let binary_path = install_dir.clone();

        // Skip download if already installed
        if !binary_path.exists() {
            // Create installation directory if it doesn't exist
            fs::create_dir_all(&install_dir).map_err(|e| {
                EnvError::FileOperation(format!("Failed to create installation directory: {}", e))
            })?;

            // Try downloading from multiple sources
            let mut last_error = None;

            // Configure HTTP client with proper redirect handling and User-Agent
            let client = reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10)) // Follow up to 10 redirects
                .user_agent("xdxtools/0.1.0 (compatible; Mozilla/5.0)") // Browser-like User-Agent
                .timeout(Duration::from_secs(300)) // 5 minute timeout for large downloads
                .build()
                .map_err(|e| EnvError::Network(format!("Failed to create HTTP client: {}", e)))?;

            for (source_name, url) in &download_urls {
                info!("Attempting to download micromamba from: {}", source_name);

                // Download with progress bar
                let pb = ProgressBar::new(0);
                let style = ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{wide_msg}]")
                    .map_err(|e| {
                        EnvError::Template(format!("Failed to create progress bar: {}", e))
                    })?;
                let style = style.tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
                pb.set_style(style);
                pb.set_message(format!("Downloading micromamba from {}...", source_name));

                match client.get(url).send() {
                    Ok(response) => {
                        if !response.status().is_success() {
                            let error_msg = format!("HTTP {}", response.status());
                            warn!("Failed to download from {}: {}", source_name, error_msg);
                            last_error = Some(format!("{}: {}", source_name, error_msg));
                            continue;
                        }

                        let total_size = match response.content_length() {
                            Some(size) => {
                                pb.set_length(size);
                                size
                            }
                            None => {
                                warn!("Could not determine content length from {}", source_name);
                                0
                            }
                        };

                        match response.bytes() {
                            Ok(bytes) => {
                                // Check if the downloaded content is HTML (indicating an error page)
                                if bytes.starts_with(b"<!DOCTYPE html")
                                    || bytes.starts_with(b"<html")
                                {
                                    warn!(
                                        "Downloaded file from {} appears to be HTML (error page)",
                                        source_name
                                    );
                                    last_error = Some(format!(
                                        "{}: Received HTML instead of binary",
                                        source_name
                                    ));
                                    continue;
                                }

                                // Check if it's a tar.bz2 archive (from official source)
                                let is_tar_bz2 = bytes.starts_with(&[0x42, 0x5a, 0x68]); // "BZh" magic bytes

                                if is_tar_bz2 {
                                    // Write to temporary file for extraction
                                    let temp_file = install_dir.join("micromamba.tar.bz2");
                                    let mut file = match fs::File::create(&temp_file) {
                                        Ok(f) => f,
                                        Err(e) => {
                                            let error_msg =
                                                format!("Failed to create temp file: {}", e);
                                            warn!("{}: {}", source_name, error_msg);
                                            last_error =
                                                Some(format!("{}: {}", source_name, error_msg));
                                            continue;
                                        }
                                    };

                                    if let Err(e) = file.write_all(&bytes) {
                                        let error_msg = format!("Failed to write temp file: {}", e);
                                        warn!("{}: {}", source_name, error_msg);
                                        last_error =
                                            Some(format!("{}: {}", source_name, error_msg));
                                        continue;
                                    }

                                    // Extract the tar.bz2 archive
                                    info!("Extracting micromamba from tar.bz2 archive...");
                                    let extract_output = AsyncCommand::new("tar")
                                        .args(&[
                                            "-xjf",
                                            temp_file.to_str().unwrap(),
                                            "-C",
                                            install_dir.to_str().unwrap(),
                                        ])
                                        .output()
                                        .await;

                                    match extract_output {
                                        Ok(output) => {
                                            if !output.status.success() {
                                                let error = String::from_utf8_lossy(&output.stderr);
                                                warn!("Failed to extract archive: {}", error);
                                                last_error = Some(format!(
                                                    "{}: Extraction failed: {}",
                                                    source_name, error
                                                ));
                                                continue;
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to run tar command: {}", e);
                                            last_error = Some(format!(
                                                "{}: Failed to run tar: {}",
                                                source_name, e
                                            ));
                                            continue;
                                        }
                                    }

                                    // Clean up temp file
                                    if let Err(e) = fs::remove_file(&temp_file) {
                                        warn!("Failed to remove temp file: {}", e);
                                    }
                                } else {
                                    // Direct binary download (GitHub)
                                    let mut file = match fs::File::create(&binary_path) {
                                        Ok(f) => f,
                                        Err(e) => {
                                            let error_msg = format!("Failed to create file: {}", e);
                                            warn!("{}: {}", source_name, error_msg);
                                            last_error =
                                                Some(format!("{}: {}", source_name, error_msg));
                                            continue;
                                        }
                                    };

                                    if let Err(e) = file.write_all(&bytes) {
                                        let error_msg = format!("Failed to write file: {}", e);
                                        warn!("{}: {}", source_name, error_msg);
                                        last_error =
                                            Some(format!("{}: {}", source_name, error_msg));
                                        continue;
                                    }
                                }

                                pb.finish_and_clear();
                                info!("Successfully downloaded micromamba from {}", source_name);

                                // Set execute permissions on Unix
                                #[cfg(unix)]
                                {
                                    let mut perms =
                                        match fs::metadata(&binary_path).map(|m| m.permissions()) {
                                            Ok(p) => p,
                                            Err(e) => {
                                                warn!("Failed to get file permissions: {}", e);
                                                continue;
                                            }
                                        };
                                    perms.set_mode(0o755);
                                    if let Err(e) = fs::set_permissions(&binary_path, perms) {
                                        warn!("Failed to set execute permissions: {}", e);
                                        // Don't fail on permission errors
                                    }
                                }

                                // Success!
                                break;
                            }
                            Err(e) => {
                                let error_msg = format!("Failed to read response: {}", e);
                                warn!("{}: {}", source_name, error_msg);
                                last_error = Some(format!("{}: {}", source_name, error_msg));
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("Connection failed: {}", e);
                        warn!("{}: {}", source_name, error_msg);
                        last_error = Some(format!("{}: {}", source_name, error_msg));
                        continue;
                    }
                }
            }

            // Handle nested directory case for tar.bz2 archives
            // Some micromamba tar.bz2 packages create a nested directory structure
            if binary_path.exists() {
                // If binary_path is a directory, we need to find the actual binary
                if binary_path.is_dir() {
                    let nested_dir = binary_path.join("bin");
                    if nested_dir.exists() && nested_dir.is_dir() {
                        let nested_binary = nested_dir.join("micromamba");
                        if nested_binary.exists() {
                            info!("Detected nested directory structure, moving binary to correct location...");
                            // Create a temporary path for the binary
                            let temp_binary_path = binary_path
                                .parent()
                                .unwrap()
                                .join("micromamba.tmp");

                            // First copy to a temporary location
                            if let Err(e) = fs::copy(&nested_binary, &temp_binary_path) {
                                warn!("Failed to copy nested binary to temp location: {}", e);
                            } else {
                                // Remove the entire nested directory
                                if let Err(e) = fs::remove_dir_all(&binary_path) {
                                    warn!("Failed to remove nested directory: {}", e);
                                    // Clean up temp file
                                    let _ = fs::remove_file(&temp_binary_path);
                                } else {
                                    // Now move the temp file to the final location
                                    if let Err(e) = fs::rename(&temp_binary_path, &binary_path) {
                                        warn!("Failed to rename temp binary: {}", e);
                                    } else {
                                        #[cfg(unix)]
                                        {
                                            if let Ok(metadata) = fs::metadata(&binary_path) {
                                                let mut perms = metadata.permissions();
                                                perms.set_mode(0o755);
                                                if let Err(e) = fs::set_permissions(&binary_path, perms) {
                                                    warn!(
                                                        "Failed to set execute permissions: {}",
                                                        e
                                                    );
                                                }
                                            }
                                        }
                                        info!("Successfully moved micromamba binary to correct location");
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Check if we successfully downloaded
            if !binary_path.exists() {
                return Err(EnvError::Network(format!(
                    "Failed to download micromamba from all sources. Tried:\n  - {}\n  - {}\n\nLast error: {}\n\nPlease install micromamba manually from https://github.com/mamba-org/micromamba-releases and add it to your PATH, or set the MICROMAMBA_PATH environment variable.",
                    download_urls[0].0,
                    download_urls[1].0,
                    last_error.unwrap_or_else(|| "Unknown error".to_string())
                )));
            }
        } else {
            info!("Micromamba binary already exists, skipping download");
        }

        Ok(binary_path)
    }

    /// Initialize environments from config directory
    async fn initialize_environments(&mut self, verbose: bool) -> Result<()> {
        // Auto-copy configuration templates if they don't exist
        self.auto_copy_config_templates(verbose).await?;

        // Load environment configurations
        let env_names = [
            "xdxtools-core",
            "xdxtools-r",
            "xdxtools-snakemake",
            "xdxtools-extra",
        ];

        for env_name in &env_names {
            let env_file = self.config_dir.join(format!("{}.yaml", env_name));

            if !env_file.exists() {
                if verbose {
                    warn!("Environment file not found: {:?}", env_file);
                }
                continue;
            }

            // Get tools for this environment
            let tools = TOOL_ENVIRONMENT_MAP
                .iter()
                .filter_map(|(tool, env)| {
                    if env == env_name {
                        Some(tool.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>();

            // Check if environment exists
            let status = match self.environment_exists(env_name).await {
                Ok(exists) => {
                    if exists {
                        EnvironmentStatus::Installed
                    } else {
                        EnvironmentStatus::NotInstalled
                    }
                }
                Err(e) => EnvironmentStatus::Error(e.to_string()),
            };

            let environment = MicromambaEnvironment {
                name: env_name.to_string(),
                file_path: env_file,
                tools,
                status,
                created_at: None,
            };

            self.environments.insert(env_name.to_string(), environment);
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
            .join("environments")
            .join("configs");

        // Check if configuration directory exists and has files
        if self.config_dir.exists() {
            let yaml_count = fs::read_dir(&self.config_dir)
                .map_err(|e| EnvError::FileOperation(format!("Failed to read config dir: {}", e)))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".yaml"))
                .count();

            if yaml_count >= 4 {
                if verbose {
                    debug!("Configuration templates already exist, skipping copy");
                }
                return Ok(());
            }
        }

        if verbose {
            info!("Auto-copying environment configuration templates...");
        }

        // Create config directory if it doesn't exist
        fs::create_dir_all(&self.config_dir)
            .map_err(|e| EnvError::FileOperation(format!("Failed to create config dir: {}", e)))?;

        // Copy all YAML files
        let files = [
            "xdxtools-core.yaml",
            "xdxtools-r.yaml",
            "xdxtools-snakemake.yaml",
            "xdxtools-extra.yaml",
        ];
        for file_name in &files {
            let source_file = source_config_dir.join(file_name);
            let target_file = self.config_dir.join(file_name);

            if source_file.exists() && !target_file.exists() {
                fs::copy(&source_file, &target_file).map_err(|e| {
                    EnvError::FileOperation(format!("Failed to copy {}: {}", file_name, e))
                })?;
                if verbose {
                    info!("✓ Copied configuration template: {}", file_name);
                }
            }
        }

        Ok(())
    }

    /// Validate micromamba installation
    async fn validate_micromamba(&self) -> Result<bool> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("--version");

        // Apply environment variables
        self.apply_env_to_command(&mut cmd);

        let output = cmd
            .output()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to execute micromamba: {}", e)))?;

        Ok(output.status.success())
    }

    /// Create environment from YAML file
    pub async fn create_environment(&self, yaml_file: &Path, dry_run: bool) -> Result<()> {
        let _lock = self.creation_lock.lock().await;

        info!("create_environment called for {:?}", yaml_file);
        let pb = ProgressBar::new_spinner();
        pb.enable_steady_tick(Duration::from_millis(120));

        if dry_run {
            pb.set_message("Validating YAML configuration...");
            let validation = self.validate_yaml(yaml_file).await?;
            println!("{}", serde_json::to_string_pretty(&validation)?);
            pb.finish_and_clear();
            return Ok(());
        }

        // Validate YAML before creating
        pb.set_message("Validating YAML configuration...");
        info!("About to validate YAML for {:?}", yaml_file);
        let _validation = self.validate_yaml(yaml_file).await?;
        info!("YAML validation complete");

        pb.set_message("Creating environment...");
        info!("Building micromamba command for {:?}", yaml_file);
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env")
            .arg("create")
            .arg("-f")
            .arg(yaml_file)
            .arg("-y")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Apply environment variables
        self.apply_env_to_command(&mut cmd);

        info!("Command built, executing...");

        // 清除进度条以避免输出混合
        pb.finish_and_clear();

        let output = cmd
            .output()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to execute micromamba: {}", e)))?;

        // Display stdout and stderr
        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            print!("{}", stdout);
        }
        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprint!("{}", stderr);
        }

        if !output.status.success() {
            // Check for specific error: "Non-conda folder exists at prefix"
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("Non-conda folder exists at prefix") {
                let error_msg = format!(
                    "Failed to create environment: Environment directory already exists but is not a valid conda environment. \
Please remove the existing directory and try again, or use a different environment name."
                );
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

        info!("Environment created successfully from {:?}", yaml_file);
        Ok(())
    }

    /// Run command in environment
    pub async fn run_in_environment(&self, env_name: &str, command: &str) -> Result<Output> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("run")
            .arg("-n")
            .arg(env_name)
            .arg("--no-capture-output")
            .arg(command);

        // Apply environment variables
        self.apply_env_to_command(&mut cmd);

        cmd.output().await.map_err(|e| {
            EnvError::Execution(format!("Failed to run command in environment: {}", e))
        })
    }

    /// Run command in environment with extended options
    pub async fn run_in_environment_extended(
        &self,
        env_name: &str,
        command: &str,
        env_vars: &[String],
        cwd: &Path,
        capture_output: bool,
    ) -> Result<()> {
        // Use detected package manager
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("run")
            .arg("-n")
            .arg(env_name);

        // Add environment variables
        for env_pair in env_vars {
            if let Some((key, value)) = env_pair.split_once('=') {
                cmd.env(key, value);
            }
        }

        // Use bash -c to execute command with proper shell features
        cmd.arg("bash")
            .arg("-c")
            .arg(command);

        // Set working directory
        cmd.current_dir(cwd);

        // Apply standard micromamba environment variables
        self.apply_env_to_command(&mut cmd);

        // Set output configuration
        if capture_output {
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
        }

        // Execute command
        let output = if capture_output {
            // Capture output to display it
            let output = cmd.output().await.map_err(|e| {
                EnvError::Execution(format!("Failed to execute command: {}", e))
            })?;

            // Display stdout
            if !output.stdout.is_empty() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                print!("{}", stdout);
            }

            // Display stderr
            if !output.stderr.is_empty() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprint!("{}", stderr);
            }

            output
        } else {
            // Don't capture, let it inherit (for real-time output)
            let status = cmd.status().await.map_err(|e| {
                EnvError::Execution(format!("Failed to execute command: {}", e))
            })?;

            // Create a mock output for status checking
            std::process::Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        };

        if !output.status.success() {
            return Err(EnvError::Execution(format!(
                "Command failed with exit code {:?}",
                output.status.code()
            )));
        }

        Ok(())
    }

    /// Check if environment exists
    pub async fn environment_exists(&self, env_name: &str) -> Result<bool> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env").arg("list").arg("--json");

        // Apply environment variables
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

        let json_str = String::from_utf8_lossy(&output.stdout);

        // Parse the JSON to check if environment exists
        #[derive(Deserialize)]
        struct EnvList {
            envs: Vec<String>,
        }

        let envs: EnvList = serde_json::from_str(&json_str).map_err(|e| {
            EnvError::Validation(format!("Failed to parse environment list: {}", e))
        })?;

        // Extract environment name from path and compare
        Ok(envs.envs.iter().any(|env| {
            Path::new(env)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == env_name)
                .unwrap_or(false)
        }))
    }

    /// Remove environment
    pub async fn remove_environment(&self, env_name: &str) -> Result<()> {
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("env")
            .arg("remove")
            .arg("-n")
            .arg(env_name)
            .arg("-y")
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        // Apply environment variables
        self.apply_env_to_command(&mut cmd);

        let status = cmd
            .status()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to remove environment: {}", e)))?;

        if !status.success() {
            return Err(EnvError::Execution(format!(
                "Failed to remove environment: exit code {:?}",
                status.code()
            )));
        }

        info!("Environment {} removed successfully", env_name);
        Ok(())
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
    pub async fn install_packages(&self, env_name: &str, packages: &[String]) -> Result<()> {
        use tokio::process::Command as AsyncCommand;

        // Use lock to prevent race conditions during package installation
        let _lock = self.creation_lock.lock().await;

        // Check if environment exists
        if !self.environment_exists(env_name).await? {
            return Err(EnvError::Execution(format!(
                "Environment '{}' does not exist. Please create it first using 'xdxtools env create --name {}'",
                env_name,
                env_name
            )));
        }

        info!("Installing packages in environment '{}': {:?}", env_name, packages);
        debug!("micromamba path: {:?}", self.pm_path);

        // Build micromamba install command with default channels
        let mut cmd = AsyncCommand::new(&self.pm_path);
        cmd.arg("install")
            .arg("-n")
            .arg(env_name)
            .arg("-c")
            .arg("conda-forge")
            .arg("-c")
            .arg("bioconda")
            .arg("-y");

        // Add all packages to install
        for package in packages {
            cmd.arg(package);
        }

        // Inherit stdout/stderr to show installation progress
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        // Apply environment variables
        self.apply_env_to_command(&mut cmd);

        let status = cmd
            .status()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to execute micromamba: {}", e)))?;

        if !status.success() {
            return Err(EnvError::Execution(format!(
                "Failed to install packages: micromamba command failed with exit code {:?}",
                status.code()
            )));
        }

        info!("Successfully installed packages in environment '{}'", env_name);
        Ok(())
    }

    /// Update environment statuses (for compatibility with CondaManager API)
    pub async fn update_environment_statuses(&mut self) -> Result<()> {
        let environment_names: Vec<String> = self.environments.keys().cloned().collect();

        for env_name in environment_names {
            if self.environment_exists(&env_name).await? {
                if let Some(environment) = self.environments.get_mut(&env_name) {
                    if environment.status != crate::micromamba::EnvironmentStatus::Ready
                    {
                        environment.status =
                            crate::micromamba::EnvironmentStatus::Ready;
                        info!("Environment '{}' is ready", env_name);
                    }
                }
            } else {
                if let Some(environment) = self.environments.get_mut(&env_name) {
                    if environment.status == crate::micromamba::EnvironmentStatus::Ready
                    {
                        environment.status =
                            crate::micromamba::EnvironmentStatus::NotInstalled;
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
            "xdxtools-core" => Ok(self.generate_xdxtools_core_yaml()),
            "xdxtools-r" => Ok(self.generate_xdxtools_r_yaml()),
            "xdxtools-snakemake" => Ok(self.generate_xdxtools_snakemake_yaml()),
            "xdxtools-extra" => Ok(self.generate_xdxtools_extra_yaml()),
            _ => Err(EnvError::Validation(format!(
                "Unknown environment: {}",
                env_name
            ))),
        }
    }

    /// Generate xdxtools-core environment YAML content
    fn generate_xdxtools_core_yaml(&self) -> String {
        r#"name: xdxtools-core
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.10
  - numpy=1.24
  - pandas
  - matplotlib
  - seaborn
  - scipy
  - scikit-learn
  - biopython
  - cutadapt
  - fastqc
  - multiqc
  - trimmomatic
  - bowtie2
  - hisat2
  - star
  - subread
  - samtools
  - bcftools
  - bedtools
  - igvtools
  - picard
  - gatk4
  - snakemake
  - pandas
  - numpy
  - matplotlib
  - seaborn
  - jupyter
"#
        .to_string()
    }

    /// Generate xdxtools-r environment YAML content
    fn generate_xdxtools_r_yaml(&self) -> String {
        r#"name: xdxtools-r
channels:
  - conda-forge
  - bioconda
dependencies:
  - r-base=4.4.3
  - qualimap
  - r-tidyverse
  - r-dplyr
  - r-ggplot2
  - r-pheatmap
  - r-rcolorbrewer
  - r-data.table
  - r-readr
  - r-stringr
  - r-matrix
  - r-genomicranges
  - r-iranges
  - r-s4vectors
  - r-biocmanager
"#
        .to_string()
    }

    /// Generate xdxtools-snakemake environment YAML content
    fn generate_xdxtools_snakemake_yaml(&self) -> String {
        r#"name: xdxtools-snakemake
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.10
  - snakemake
  - pandas
  - numpy
  - matplotlib
  - graphviz
  - pyyaml
  - docutils
  - jinja2
  - setuptools
"#
        .to_string()
    }

    /// Generate xdxtools-extra environment YAML content
    fn generate_xdxtools_extra_yaml(&self) -> String {
        r#"name: xdxtools-extra
channels:
  - conda-forge
  - bioconda
dependencies:
  - python=3.10
  - plotly
  - dash
  - bokeh
  - altair
  - streamlit
  - dash-bootstrap-components
  - openpyxl
  - xlsxwriter
  - pillow
  - networkx
  - python-igraph
"#
        .to_string()
    }

    /// Get all conda environments from the system
    ///
    /// This function executes `conda env list` (or mamba/micromamba) and parses the output
    /// to return a list of all conda environments with their names and prefixes.
    pub async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>> {
        let cmd_name = match &self.pm_type {
            PackageManager::Conda => "conda",
            PackageManager::Mamba => "mamba",
            PackageManager::Micromamba => "micromamba",
            PackageManager::None => {
                return Ok(vec![]);
            }
        };

        debug!("Executing {} env list", cmd_name);

        // Execute conda env list
        let output = tokio::process::Command::new(cmd_name)
            .args(&["env", "list"])
            .output()
            .await
            .map_err(|e| EnvError::Execution(format!("Failed to execute {}: {}", cmd_name, e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EnvError::Execution(format!(
                "Failed to list environments: {}",
                stderr
            )));
        }

        // Parse the output
        parse_conda_env_list(&output.stdout)
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
}

/// Parse conda env list output
///
/// Expected format:
/// ```text
/// # conda environments:
/// #
/// base                  * /path/to/miniconda3
/// env1                     /path/to/miniconda3/envs/env1
/// env2                     /path/to/miniconda3/envs/env2
/// ```
fn parse_conda_env_list(output: &[u8]) -> Result<Vec<CondaEnvironment>> {
    let content = String::from_utf8_lossy(output);
    let mut environments = Vec::new();

    for line in content.lines() {
        // Skip comments and empty lines
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }

        // Parse line: name * /path/to/env
        // The second field (*) indicates the active environment
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() >= 2 {
            let name = parts[0].to_string();

            // Check if this is the active environment
            let is_active = parts.get(1).map(|&s| s == "*").unwrap_or(false);

            // The prefix is the last field (after potential * marker)
            // Format: name [*] prefix
            let prefix = if is_active {
                // "name * /path" -> parts.len() >= 3
                if parts.len() >= 3 {
                    parts[2].to_string()
                } else {
                    continue; // Invalid format
                }
            } else {
                // "name /path" -> parts.len() >= 2
                parts[1].to_string()
            };

            environments.push(CondaEnvironment {
                name,
                prefix,
                is_active,
            });
        }
    }

    Ok(environments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_validate_yaml() {
        let temp_dir = tempfile::tempdir().unwrap();
        let yaml_file = temp_dir.path().join("test.yaml");

        // Create a simple test YAML
        let yaml_content = r#"
name: test-env
channels:
  - conda-forge
dependencies:
  - python=3.10
  - numpy
"#;
        fs::write(&yaml_file, yaml_content).unwrap();

        let manager = MicromambaManager::with_config_dir(temp_dir.path())
            .await
            .unwrap();

        let result = manager.validate_yaml(&yaml_file).await.unwrap();

        assert!(result.validation.syntax_valid);
        assert_eq!(result.environment, "test-env");
        assert!(result.estimated_packages >= 2);
    }

    #[tokio::test]
    async fn test_environment_list() {
        let temp_dir = tempfile::tempdir().unwrap();

        let manager = MicromambaManager::with_config_dir(temp_dir.path())
            .await
            .unwrap();
        let envs = manager.list_environments().await.unwrap();

        // Should have at least the 4 core environments defined
        assert!(envs.len() >= 4);
    }
}
