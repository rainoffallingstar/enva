//! enva - A rattler-first environment manager for bioinformatics workflows

pub mod backend;
pub mod env;
pub mod env_run;
pub mod error;
pub mod micromamba;
mod ownership;
pub mod package_manager;
mod prefix_registry;

// Re-export commonly used types
pub use backend::{BackendKind, BackendSelector, OutputMode};
pub use env::{execute_env_command, EnvArgs};
pub use error::{EnvError, Result};
pub use package_manager::{get_global_detector, PackageManager, PackageManagerDetector};

// Constants for the 3 core environments
pub const CORE_ENV_NAME: &str = "xdxtools-core";
pub const SNAKEMAKE_ENV_NAME: &str = "xdxtools-snakemake";
pub const EXTRA_ENV_NAME: &str = "xdxtools-extra";

/// Initialize enva library.
///
/// This only performs process-local setup and does not require any compatibility
/// package manager to be available.
pub async fn initialize() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    Ok(())
}

/// Display startup banner
pub fn display_startup_banner() {
    println!(
        r#"#========================================#
#       enva v0.1.0                        #
#  Rattler-First Env Manager               #
#  For Bioinformatics Workflows            #
#========================================#
"#
    );
}

#[cfg(test)]
mod tests {
    use super::initialize;

    #[tokio::test]
    async fn initialize_is_idempotent_without_package_manager_probe() {
        initialize().await.unwrap();
        initialize().await.unwrap();
    }
}
