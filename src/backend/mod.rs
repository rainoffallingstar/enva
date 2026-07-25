pub mod cli;
pub mod factory;
pub mod rattler;
pub mod types;

use crate::error::{EnvError, Result};
use crate::micromamba::{CondaEnvironment, ValidationResult};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command as AsyncCommand;
pub use types::{
    BackendCapabilities, BackendCapability, BackendKind, BackendSelector, CapabilitySupport,
    EnvironmentName, EnvironmentResolution, EnvironmentTarget, OutputMode, RunCommand, RunRequest,
};

pub(crate) const ENVIRONMENT_SHELL: &str = "bash";

pub(crate) fn append_environment_shell_arguments(parent_command: &mut AsyncCommand, command: &str) {
    parent_command.arg(ENVIRONMENT_SHELL).arg("-c").arg(command);
}

pub(crate) fn build_environment_shell_command(command: &str) -> AsyncCommand {
    let mut shell_command = AsyncCommand::new(ENVIRONMENT_SHELL);
    shell_command.arg("-c").arg(command);
    shell_command
}

pub(crate) fn append_environment_run_command(
    parent_command: &mut AsyncCommand,
    command: &RunCommand,
) -> Result<()> {
    match command {
        RunCommand::Argv(arguments) => {
            if arguments.is_empty() || arguments[0].is_empty() {
                return Err(EnvError::Validation(
                    "Argv command must contain a non-empty program".to_string(),
                ));
            }
            parent_command.args(arguments);
        }
        RunCommand::Shell(command) => append_environment_shell_arguments(parent_command, command),
    }

    Ok(())
}

pub(crate) fn build_environment_run_command(command: &RunCommand) -> Result<AsyncCommand> {
    match command {
        RunCommand::Argv(arguments) => {
            let (program, program_arguments) = arguments.split_first().ok_or_else(|| {
                EnvError::Validation("Argv command must contain a non-empty program".to_string())
            })?;
            if program.is_empty() {
                return Err(EnvError::Validation(
                    "Argv command must contain a non-empty program".to_string(),
                ));
            }

            let mut process = AsyncCommand::new(program);
            process.args(program_arguments);
            Ok(process)
        }
        RunCommand::Shell(command) => Ok(build_environment_shell_command(command)),
    }
}

#[async_trait]
pub trait EnvironmentBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    fn capabilities(&self) -> BackendCapabilities;

    fn require_capability(&self, capability: BackendCapability) -> Result<CapabilitySupport> {
        self.capabilities().require(self.kind(), capability)
    }

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

    async fn validate_yaml_with_packages(
        &self,
        yaml_file: &Path,
        additional_packages: &[String],
    ) -> Result<ValidationResult> {
        if !additional_packages.is_empty() {
            return Err(EnvError::Validation(
                "The selected backend cannot include additional package specs in YAML validation"
                    .to_string(),
            ));
        }
        self.validate_yaml(yaml_file).await
    }

    async fn environment_exists(&self, env_name: &str) -> Result<bool>;

    async fn install_packages(
        &self,
        env_name: &str,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn install_packages_for_target(
        &self,
        target: &EnvironmentTarget,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()> {
        match target {
            EnvironmentTarget::Name(env_name) => {
                self.install_packages(env_name, packages, output_mode).await
            }
            EnvironmentTarget::Prefix(prefix) => Err(EnvError::Execution(format!(
                "The selected backend does not support package installation by explicit prefix: {}",
                prefix.display()
            ))),
        }
    }

    async fn adopt_environment(
        &self,
        target: &EnvironmentTarget,
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn remove_environment_with_output(
        &self,
        env_name: &str,
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn remove_environment_by_prefix_with_output(
        &self,
        prefix: &Path,
        output_mode: OutputMode,
    ) -> Result<()>;

    async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>>;

    async fn find_environment_prefixes(&self, env_name: &str) -> Result<Vec<PathBuf>>;

    async fn run(&self, target: &EnvironmentTarget, request: &RunRequest) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{
        append_environment_run_command, append_environment_shell_arguments,
        build_environment_run_command, build_environment_shell_command, RunCommand,
    };
    use std::ffi::OsString;
    use tokio::process::Command as AsyncCommand;

    #[test]
    fn environment_shell_command_is_non_login_bash() {
        let shell_command = build_environment_shell_command("printf ready");
        let arguments = shell_command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string())
            .collect::<Vec<String>>();

        assert_eq!(shell_command.as_std().get_program(), "bash");
        assert_eq!(arguments, vec!["-c", "printf ready"]);
    }

    #[test]
    fn appended_environment_shell_arguments_are_non_login_bash() {
        let mut parent_command = AsyncCommand::new("conda");
        append_environment_shell_arguments(&mut parent_command, "printf ready");
        let arguments = parent_command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string())
            .collect::<Vec<String>>();

        assert_eq!(arguments, vec!["bash", "-c", "printf ready"]);
    }

    #[test]
    fn appended_argv_arguments_keep_exact_boundaries() {
        let expected_arguments = vec![
            OsString::from("tool"),
            OsString::from("value with spaces"),
            OsString::from("semi;colon"),
            OsString::from("$(printf injected)"),
            OsString::from("*.fastq.gz"),
            OsString::from("line one\nline two"),
            OsString::from("-leading-option"),
        ];
        let mut parent_command = AsyncCommand::new("conda");

        append_environment_run_command(
            &mut parent_command,
            &RunCommand::Argv(expected_arguments.clone()),
        )
        .unwrap();

        assert_eq!(
            parent_command
                .as_std()
                .get_args()
                .map(OsString::from)
                .collect::<Vec<OsString>>(),
            expected_arguments
        );
    }

    #[test]
    fn direct_argv_command_keeps_program_and_arguments_separate() {
        let command = build_environment_run_command(&RunCommand::Argv(vec![
            OsString::from("tool"),
            OsString::from("value with spaces"),
            OsString::from("semi;colon"),
        ]))
        .unwrap();

        assert_eq!(command.as_std().get_program(), "tool");
        assert_eq!(
            command
                .as_std()
                .get_args()
                .map(OsString::from)
                .collect::<Vec<OsString>>(),
            vec![
                OsString::from("value with spaces"),
                OsString::from("semi;colon")
            ]
        );
    }
}
