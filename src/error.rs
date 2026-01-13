//! Error handling for enva

use chrono::DateTime;
use std::path::PathBuf;
use thiserror::Error;

/// Custom error types for enva
#[derive(Error, Debug)]
pub enum EnvError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("File operation failed: {0}")]
    FileOperation(String),

    #[error("Environment error: {0}")]
    Environment(String),

    #[error("Resource allocation failed: {0}")]
    Resource(String),

    #[error("Lock error: {0}")]
    Lock(String),

    #[error("Workflow execution failed: {0}")]
    Workflow(String),

    #[error("Template rendering failed: {0}")]
    Template(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Download failed: {0}")]
    DownloadFailed(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Dependency error: {0}")]
    Dependency(String),

    #[error("Process spawn error: {0}")]
    ProcessSpawn(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Package installation failed: {0}")]
    InstallationFailed(String),

    #[error("Package management error: {0}")]
    PackageManagement(String),

    #[error("System error: {0}")]
    System(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parsing error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parsing error: {0}")]
    Toml(String),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("Chrono error: {0}")]
    Chrono(#[from] chrono::ParseError),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EnvError>;

impl EnvError {
    /// Add context to the error for better debugging
    pub fn with_context(mut self, context: &str) -> Self {
        match &mut self {
            EnvError::Config(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::Workflow(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::Environment(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::Resource(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::Template(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::Validation(msg) => *msg = format!("{}: {}", context, msg),
            EnvError::FileOperation(msg) => *msg = format!("{}: {}", context, msg),
            _ => self = EnvError::Internal(format!("{}: {}", context, self)),
        }
        self
    }

    /// Check if this is a recoverable error
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            EnvError::Workflow(_)
                | EnvError::Resource(_)
                | EnvError::Network(_)
                | EnvError::Environment(_)
        )
    }

    /// Check if this error indicates missing dependencies
    pub fn is_dependency_error(&self) -> bool {
        matches!(
            self,
            EnvError::Environment(_) | EnvError::System(_) | EnvError::PermissionDenied(_)
        )
    }

    /// Get error severity level
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            EnvError::Config(_) | EnvError::Validation(_) | EnvError::InvalidInput(_) => {
                ErrorSeverity::Warning
            }
            EnvError::Dependency(_) | EnvError::Resource(_) | EnvError::PermissionDenied(_) => {
                ErrorSeverity::Error
            }
            EnvError::FileNotFound(_) | EnvError::Workflow(_) => {
                ErrorSeverity::Critical
            }
            _ => ErrorSeverity::Error,
        }
    }

    /// Get error timestamp
    pub fn timestamp(&self) -> DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

/// Error severity levels
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorSeverity {
    Warning,
    Error,
    Critical,
}

/// Error context information
#[derive(Debug, Clone)]
pub struct ErrorContext {
    pub workflow_id: Option<String>,
    pub step: Option<u32>,
    pub operation: String,
    pub timestamp: DateTime<chrono::Utc>,
}

impl ErrorContext {
    pub fn new(operation: &str) -> Self {
        Self {
            workflow_id: None,
            step: None,
            operation: operation.to_string(),
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn with_workflow_id(mut self, workflow_id: &str) -> Self {
        self.workflow_id = Some(workflow_id.to_string());
        self
    }

    pub fn with_step(mut self, step: u32) -> Self {
        self.step = Some(step);
        self
    }
}

/// Enhanced error with context
#[derive(Debug)]
pub struct ContextualError {
    pub error: EnvError,
    pub context: ErrorContext,
}

impl ContextualError {
    pub fn new(error: EnvError, context: ErrorContext) -> Self {
        Self { error, context }
    }

    pub fn from_error(error: EnvError, operation: &str) -> Self {
        Self {
            error,
            context: ErrorContext::new(operation),
        }
    }
}

impl std::fmt::Display for ContextualError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {}",
            self.context.timestamp.format("%Y-%m-%d %H:%M:%S"),
            self.error
        )
    }
}

impl std::error::Error for ContextualError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Result type with context
pub type ContextualResult<T> = std::result::Result<T, ContextualError>;

/// Extension trait for Result types to add context
pub trait ResultExt<T> {
    fn with_context(self, operation: &str) -> ContextualResult<T>;
}

impl<T> ResultExt<T> for Result<T> {
    fn with_context(self, operation: &str) -> ContextualResult<T> {
        match self {
            Ok(value) => Ok(value),
            Err(error) => Err(ContextualError::from_error(error, operation)),
        }
    }
}
