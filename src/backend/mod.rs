pub mod cli;
pub mod factory;
pub mod rattler;
pub mod types;

use crate::error::Result;
use crate::micromamba::{CondaEnvironment, ValidationResult};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
pub use types::{BackendKind, BackendSelector, EnvironmentTarget, OutputMode, RunRequest};

#[async_trait]
pub trait EnvironmentBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    async fn clean_package_cache(&self, dry_run: bool, output_mode: OutputMode) -> Result<()>;

    async fn create_environment(
        &self,
        env_name: &str,
        yaml_file: &Path,
        dry_run: bool,
        force: bool,
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn validate_yaml(&self, yaml_file: &Path) -> Result<ValidationResult>;

    async fn environment_exists(&self, env_name: &str) -> Result<bool>;

    async fn install_packages(&self, env_name: &str, packages: &[String]) -> Result<()>;

    async fn remove_environment_with_output(
        &self,
        env_name: &str,
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>>;

    async fn find_environment_prefixes(&self, env_name: &str) -> Result<Vec<PathBuf>>;

    async fn run(&self, target: &EnvironmentTarget, request: &RunRequest) -> Result<()>;
}
