//! enva - A rattler-first environment manager for bioinformatics workflows

pub mod backend;
pub mod env;
pub mod env_run;
pub mod error;
pub mod micromamba;
mod operation_lock;
mod ownership;
pub mod package_manager;
mod prefix_registry;
mod staged_prefix;

// Re-export commonly used types
pub use backend::{BackendKind, BackendSelector, OutputMode};
pub use env::{execute_env_command, EnvArgs};
pub use error::{EnvError, Result};
pub use package_manager::{get_global_detector, PackageManager, PackageManagerDetector};

// Constants for the 3 core environments
pub const CORE_ENV_NAME: &str = "otter-core";
pub const SNAKEMAKE_ENV_NAME: &str = "otter-snakemake";
pub const EXTRA_ENV_NAME: &str = "otter-extra";
pub const BUILT_IN_ENV_NAMES: [&str; 3] = [CORE_ENV_NAME, SNAKEMAKE_ENV_NAME, EXTRA_ENV_NAME];

/// Initialize enva library.
///
/// This only performs process-local setup and does not require any compatibility
/// package manager to be available.
pub async fn initialize() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    Ok(())
}

/// Display the interactive startup banner on standard error.
pub fn display_startup_banner() {
    eprintln!(
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
    use super::{BUILT_IN_ENV_NAMES, CORE_ENV_NAME, EXTRA_ENV_NAME, SNAKEMAKE_ENV_NAME};

    #[test]
    fn built_in_environment_names_use_otter_branding() {
        assert_eq!(CORE_ENV_NAME, "otter-core");
        assert_eq!(SNAKEMAKE_ENV_NAME, "otter-snakemake");
        assert_eq!(EXTRA_ENV_NAME, "otter-extra");
        assert_eq!(
            BUILT_IN_ENV_NAMES,
            [CORE_ENV_NAME, SNAKEMAKE_ENV_NAME, EXTRA_ENV_NAME]
        );
    }

    #[tokio::test]
    async fn initialize_is_idempotent_without_package_manager_probe() {
        initialize().await.unwrap();
        initialize().await.unwrap();
    }
}
