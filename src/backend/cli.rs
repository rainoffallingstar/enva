use super::{BackendKind, EnvironmentBackend, EnvironmentTarget, OutputMode, RunRequest};
use crate::error::{EnvError, Result};
use crate::micromamba::{CondaEnvironment, MicromambaManager, ValidationResult};
use crate::package_manager::PackageManager;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy)]
pub struct CliBackend {
    package_manager: Option<PackageManager>,
}

impl CliBackend {
    pub fn new(package_manager: Option<PackageManager>) -> Self {
        Self { package_manager }
    }

    async fn global_manager(&self) -> Result<Arc<Mutex<MicromambaManager>>> {
        MicromambaManager::get_global_manager()
            .await
            .map_err(|error| {
                EnvError::Execution(format!("Failed to initialize CLI conda backend: {}", error))
            })
    }

    async fn runtime_manager(&self) -> Result<MicromambaManager> {
        match self.package_manager {
            Some(package_manager) => {
                MicromambaManager::new_runtime_with_package_manager(package_manager).await
            }
            None => {
                let manager = self.global_manager().await?;
                let guard = manager.lock().await;
                let cloned = guard.clone();
                Ok(cloned)
            }
        }
    }
}

#[async_trait]
impl EnvironmentBackend for CliBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Cli
    }

    async fn clean_package_cache(&self, dry_run: bool, output_mode: OutputMode) -> Result<()> {
        let manager = self.runtime_manager().await?;
        manager.clean_package_cache(dry_run, output_mode).await
    }

    async fn create_environment(
        &self,
        env_name: &str,
        yaml_file: &Path,
        dry_run: bool,
        force: bool,
        output_mode: OutputMode,
    ) -> Result<()> {
        let manager = self.runtime_manager().await?;
        manager
            .create_environment(env_name, yaml_file, dry_run, force, output_mode)
            .await
    }

    async fn validate_yaml(&self, yaml_file: &Path) -> Result<ValidationResult> {
        let manager = self.runtime_manager().await?;
        manager.validate_yaml(yaml_file).await
    }

    async fn environment_exists(&self, env_name: &str) -> Result<bool> {
        let manager = self.runtime_manager().await?;
        manager.environment_exists(env_name).await
    }

    async fn install_packages(&self, env_name: &str, packages: &[String]) -> Result<()> {
        let manager = self.runtime_manager().await?;
        manager.install_packages(env_name, packages).await
    }

    async fn adopt_environment(
        &self,
        _target: &EnvironmentTarget,
        _output_mode: OutputMode,
    ) -> Result<()> {
        Err(EnvError::Execution(
            "adopt is only supported by the rattler backend".to_string(),
        ))
    }

    async fn remove_environment_with_output(
        &self,
        env_name: &str,
        output_mode: OutputMode,
    ) -> Result<()> {
        let manager = self.runtime_manager().await?;
        manager
            .remove_environment_with_output(env_name, output_mode)
            .await
    }

    async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>> {
        let manager = self.runtime_manager().await?;
        manager.get_all_conda_environments().await
    }

    async fn find_environment_prefixes(&self, env_name: &str) -> Result<Vec<PathBuf>> {
        let manager = self.runtime_manager().await?;
        manager.find_environment_prefixes(env_name).await
    }

    async fn run(&self, target: &EnvironmentTarget, request: &RunRequest) -> Result<()> {
        let manager = self.runtime_manager().await?;
        match target {
            EnvironmentTarget::Name(env_name) => {
                manager
                    .run_in_environment_extended(
                        env_name,
                        &request.command,
                        &request.env_vars,
                        &request.cwd,
                        request.capture_output,
                    )
                    .await
            }
            EnvironmentTarget::Prefix(prefix) => {
                manager
                    .run_in_environment_by_prefix_extended(
                        prefix,
                        &request.command,
                        &request.env_vars,
                        &request.cwd,
                        request.capture_output,
                    )
                    .await
            }
        }
    }
}
