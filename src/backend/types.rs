use crate::error::{EnvError, Result};
use crate::package_manager::PackageManager;
use clap::ValueEnum;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputMode {
    Stream,
    #[default]
    Summary,
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cli,
    Rattler,
}

impl BackendKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cli => "cli compatibility",
            Self::Rattler => "rattler",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCapability {
    CleanPackageCache,
    CreateEnvironment,
    ValidateYaml,
    ValidateYamlWithPackages,
    InstallByName,
    InstallByPrefix,
    AdoptEnvironment,
    RemoveByName,
    RemoveByPrefix,
    DiscoverEnvironments,
    RunByName,
    RunByPrefix,
}

impl fmt::Display for BackendCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::CleanPackageCache => "clean package cache",
            Self::CreateEnvironment => "create environment",
            Self::ValidateYaml => "validate YAML",
            Self::ValidateYamlWithPackages => "validate YAML with additional packages",
            Self::InstallByName => "install packages by name",
            Self::InstallByPrefix => "install packages by prefix",
            Self::AdoptEnvironment => "adopt environment",
            Self::RemoveByName => "remove environment by name",
            Self::RemoveByPrefix => "remove environment by prefix",
            Self::DiscoverEnvironments => "discover environments",
            Self::RunByName => "run by environment name",
            Self::RunByPrefix => "run by environment prefix",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySupport {
    Native,
    Delegated,
    Hybrid,
    Unsupported,
}

impl CapabilitySupport {
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub clean_package_cache: CapabilitySupport,
    pub create_environment: CapabilitySupport,
    pub validate_yaml: CapabilitySupport,
    pub validate_yaml_with_packages: CapabilitySupport,
    pub install_by_name: CapabilitySupport,
    pub install_by_prefix: CapabilitySupport,
    pub adopt_environment: CapabilitySupport,
    pub remove_by_name: CapabilitySupport,
    pub remove_by_prefix: CapabilitySupport,
    pub discover_environments: CapabilitySupport,
    pub run_by_name: CapabilitySupport,
    pub run_by_prefix: CapabilitySupport,
}

impl BackendCapabilities {
    pub const fn rattler() -> Self {
        Self {
            clean_package_cache: CapabilitySupport::Native,
            create_environment: CapabilitySupport::Native,
            validate_yaml: CapabilitySupport::Native,
            validate_yaml_with_packages: CapabilitySupport::Native,
            install_by_name: CapabilitySupport::Hybrid,
            install_by_prefix: CapabilitySupport::Hybrid,
            adopt_environment: CapabilitySupport::Native,
            remove_by_name: CapabilitySupport::Hybrid,
            remove_by_prefix: CapabilitySupport::Hybrid,
            discover_environments: CapabilitySupport::Hybrid,
            run_by_name: CapabilitySupport::Native,
            run_by_prefix: CapabilitySupport::Native,
        }
    }

    pub const fn cli_compatibility() -> Self {
        Self {
            clean_package_cache: CapabilitySupport::Delegated,
            create_environment: CapabilitySupport::Delegated,
            validate_yaml: CapabilitySupport::Delegated,
            validate_yaml_with_packages: CapabilitySupport::Unsupported,
            install_by_name: CapabilitySupport::Delegated,
            install_by_prefix: CapabilitySupport::Delegated,
            adopt_environment: CapabilitySupport::Unsupported,
            remove_by_name: CapabilitySupport::Delegated,
            remove_by_prefix: CapabilitySupport::Delegated,
            discover_environments: CapabilitySupport::Delegated,
            run_by_name: CapabilitySupport::Delegated,
            run_by_prefix: CapabilitySupport::Delegated,
        }
    }

    pub fn support(self, capability: BackendCapability) -> CapabilitySupport {
        match capability {
            BackendCapability::CleanPackageCache => self.clean_package_cache,
            BackendCapability::CreateEnvironment => self.create_environment,
            BackendCapability::ValidateYaml => self.validate_yaml,
            BackendCapability::ValidateYamlWithPackages => self.validate_yaml_with_packages,
            BackendCapability::InstallByName => self.install_by_name,
            BackendCapability::InstallByPrefix => self.install_by_prefix,
            BackendCapability::AdoptEnvironment => self.adopt_environment,
            BackendCapability::RemoveByName => self.remove_by_name,
            BackendCapability::RemoveByPrefix => self.remove_by_prefix,
            BackendCapability::DiscoverEnvironments => self.discover_environments,
            BackendCapability::RunByName => self.run_by_name,
            BackendCapability::RunByPrefix => self.run_by_prefix,
        }
    }

    pub fn require(
        self,
        backend_kind: BackendKind,
        capability: BackendCapability,
    ) -> Result<CapabilitySupport> {
        let support = self.support(capability);
        if support.is_supported() {
            Ok(support)
        } else {
            Err(EnvError::Execution(format!(
                "The {} backend does not support operation: {}",
                backend_kind.label(),
                capability
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSelector {
    pub kind: BackendKind,
    pub package_manager: Option<PackageManager>,
}

impl BackendSelector {
    pub fn from_env() -> Self {
        let kind = match std::env::var("ENVA_BACKEND") {
            Ok(value) if value.eq_ignore_ascii_case("cli") => BackendKind::Cli,
            Ok(value) if value.eq_ignore_ascii_case("rattler") => BackendKind::Rattler,
            _ => BackendKind::Rattler,
        };

        Self {
            kind,
            package_manager: None,
        }
    }

    pub fn cli(package_manager: Option<PackageManager>) -> Self {
        Self {
            kind: BackendKind::Cli,
            package_manager,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvironmentName(String);

impl EnvironmentName {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let path = Path::new(&value);
        let mut components = path.components();
        let is_single_normal_component = matches!(components.next(), Some(Component::Normal(component)) if component == OsStr::new(&value))
            && components.next().is_none();
        let has_windows_drive_or_prefix = value.contains(':');

        if value.is_empty()
            || value.eq_ignore_ascii_case("base")
            || value.contains('/')
            || value.contains('\\')
            || value.contains('\0')
            || path.is_absolute()
            || has_windows_drive_or_prefix
            || !is_single_normal_component
        {
            return Err(EnvError::Validation(format!(
                "Invalid environment name '{}': expected one normal path component and rejected empty, base, absolute, dot, parent, separator, NUL, or Windows-prefixed names",
                value.escape_debug()
            )));
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for EnvironmentName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommand {
    Argv(Vec<OsString>),
    Shell(String),
}

impl RunCommand {
    pub fn argv(arguments: Vec<OsString>) -> Result<Self> {
        if arguments.is_empty() || arguments[0].is_empty() {
            return Err(EnvError::Validation(
                "Argv command must contain a non-empty program".to_string(),
            ));
        }

        Ok(Self::Argv(arguments))
    }

    pub fn shell(command: impl Into<String>) -> Result<Self> {
        let command = command.into();
        if command.is_empty() {
            return Err(EnvError::Validation(
                "Shell command must not be empty".to_string(),
            ));
        }

        Ok(Self::Shell(command))
    }

    pub fn display_lossy(&self) -> String {
        match self {
            Self::Argv(arguments) => arguments
                .iter()
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            Self::Shell(command) => command.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentResolution<T> {
    NotFound,
    Unique(T),
    Ambiguous(Vec<T>),
}

impl<T> EnvironmentResolution<T> {
    pub fn from_candidates(mut candidates: Vec<T>) -> Self {
        match candidates.len() {
            0 => Self::NotFound,
            1 => Self::Unique(candidates.remove(0)),
            _ => Self::Ambiguous(candidates),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentTarget {
    Name(String),
    Prefix(PathBuf),
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub command: RunCommand,
    pub env_vars: Vec<String>,
    pub cwd: PathBuf,
    pub capture_output: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        BackendCapabilities, BackendCapability, BackendKind, BackendSelector, CapabilitySupport,
        EnvironmentName, EnvironmentResolution, RunCommand,
    };
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_backend_env<T>(value: Option<&str>, operation: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var("ENVA_BACKEND").ok();

        match value {
            Some(value) => std::env::set_var("ENVA_BACKEND", value),
            None => std::env::remove_var("ENVA_BACKEND"),
        }

        let result = operation();

        match previous {
            Some(value) => std::env::set_var("ENVA_BACKEND", value),
            None => std::env::remove_var("ENVA_BACKEND"),
        }

        result
    }

    #[test]
    fn backend_selector_defaults_to_rattler() {
        with_backend_env(None, || {
            assert_eq!(BackendSelector::from_env().kind, BackendKind::Rattler);
        });
    }

    #[test]
    fn backend_selector_accepts_cli_override() {
        with_backend_env(Some("cli"), || {
            assert_eq!(BackendSelector::from_env().kind, BackendKind::Cli);
        });
    }

    #[test]
    fn backend_selector_accepts_rattler_override() {
        with_backend_env(Some("rattler"), || {
            assert_eq!(BackendSelector::from_env().kind, BackendKind::Rattler);
        });
    }

    #[test]
    fn capability_matrix_distinguishes_native_and_delegated_operations() {
        let rattler = BackendCapabilities::rattler();
        assert_eq!(
            rattler.support(BackendCapability::CreateEnvironment),
            CapabilitySupport::Native
        );
        assert_eq!(
            rattler.support(BackendCapability::RemoveByPrefix),
            CapabilitySupport::Hybrid
        );

        let compatibility = BackendCapabilities::cli_compatibility();
        assert_eq!(
            compatibility.support(BackendCapability::RemoveByPrefix),
            CapabilitySupport::Delegated
        );
        assert_eq!(
            compatibility.support(BackendCapability::AdoptEnvironment),
            CapabilitySupport::Unsupported
        );
    }

    #[test]
    fn unsupported_capability_fails_before_backend_initialization() {
        let error = BackendCapabilities::cli_compatibility()
            .require(BackendKind::Cli, BackendCapability::AdoptEnvironment)
            .unwrap_err();

        assert!(error.to_string().contains("cli compatibility backend"));
        assert!(error.to_string().contains("adopt environment"));
    }

    #[test]
    fn environment_name_accepts_one_normal_component() {
        let environment_name = EnvironmentName::parse("rna-seq_01").unwrap();
        assert_eq!(environment_name.as_str(), "rna-seq_01");
    }

    #[test]
    fn environment_name_rejects_unsafe_components_and_prefixes() {
        for invalid_name in [
            "",
            "base",
            "BASE",
            ".",
            "..",
            "/tmp/victim",
            "../victim",
            "nested/victim",
            r"nested\victim",
            "victim\0suffix",
            "C:",
            "C:relative",
            r"C:\victim",
            r"\\server\share",
            r"\\?\C:\victim",
        ] {
            assert!(
                EnvironmentName::parse(invalid_name).is_err(),
                "unsafe environment name should be rejected: {invalid_name:?}"
            );
        }
    }

    #[test]
    fn environment_resolution_preserves_all_ambiguous_candidates() {
        assert_eq!(
            EnvironmentResolution::from_candidates(vec!["first", "second"]),
            EnvironmentResolution::Ambiguous(vec!["first", "second"])
        );
        assert_eq!(
            EnvironmentResolution::from_candidates(vec!["only"]),
            EnvironmentResolution::Unique("only")
        );
        assert_eq!(
            EnvironmentResolution::<&str>::from_candidates(Vec::new()),
            EnvironmentResolution::NotFound
        );
    }

    #[test]
    fn argv_command_preserves_argument_boundaries() {
        let arguments = vec![
            OsString::from("tool"),
            OsString::from("value with spaces"),
            OsString::from("semi;colon"),
            OsString::from("$(printf injected)"),
            OsString::from("*.fastq.gz"),
            OsString::from("line one\nline two"),
            OsString::from("-leading-option"),
        ];

        assert_eq!(
            RunCommand::argv(arguments.clone()).unwrap(),
            RunCommand::Argv(arguments)
        );
    }
}
