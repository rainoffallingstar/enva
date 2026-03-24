use crate::package_manager::PackageManager;
use clap::ValueEnum;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputMode {
    Stream,
    Summary,
    Quiet,
}

impl Default for OutputMode {
    fn default() -> Self {
        Self::Summary
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cli,
    Rattler,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentTarget {
    Name(String),
    Prefix(PathBuf),
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub command: String,
    pub env_vars: Vec<String>,
    pub cwd: PathBuf,
    pub capture_output: bool,
}

#[cfg(test)]
mod tests {
    use super::{BackendKind, BackendSelector};
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
}
