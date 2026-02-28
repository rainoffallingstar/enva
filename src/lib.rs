//! enva - A lightweight micromamba environment manager for bioinformatics workflows

pub mod error;
pub mod package_manager;
pub mod micromamba;
pub mod env;
pub mod env_run;

// Re-export commonly used types
pub use error::{EnvError, Result};
pub use env::{EnvArgs, execute_env_command};
pub use package_manager::{PackageManager, PackageManagerDetector, get_global_detector};

// Constants for the 3 core environments
pub const CORE_ENV_NAME: &str = "xdxtools-core";
pub const SNAKEMAKE_ENV_NAME: &str = "xdxtools-snakemake";
pub const EXTRA_ENV_NAME: &str = "xdxtools-extra";

/// Initialize enva library
/// This function can be used to perform any one-time initialization
pub async fn initialize() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Verify micromamba is available
    let manager = micromamba::MicromambaManager::get_global_manager().await?;
    let _manager = manager.lock().await;

    Ok(())
}

/// Display startup banner
pub fn display_startup_banner() {
    println!(r#"#========================================#
#       enva v0.1.0                        #
#  Micromamba Environment Manager          #
#  For Bioinformatics Workflows            #
#========================================#
"#);
}
