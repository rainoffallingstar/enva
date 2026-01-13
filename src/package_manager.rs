//! Package manager detection and abstraction
//!
//! Auto-detects and prioritizes: conda → mamba → micromamba
//! - conda: Most compatible, slowest
//! - mamba: 2-3x faster than conda
//! - micromamba: 3-5x faster, lightweight

use crate::error::{Result, EnvError};
use std::process::Command;
use tracing::{debug, info, warn};
use which::which;

/// Package manager type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Conda,
    Mamba,
    Micromamba,
    None,
}

impl PackageManager {
    /// Get command name
    pub fn command(&self) -> &str {
        match self {
            PackageManager::Conda => "conda",
            PackageManager::Mamba => "mamba",
            PackageManager::Micromamba => "micromamba",
            PackageManager::None => "",
        }
    }

    /// Get run command syntax: e.g., "conda run -n env"
    pub fn run_syntax(&self, env: &str) -> String {
        match self {
            PackageManager::Conda => format!("conda run -n {}", env),
            PackageManager::Mamba => format!("mamba run -n {}", env),
            PackageManager::Micromamba => format!("micromamba run -n {}", env),
            PackageManager::None => env.to_string(),
        }
    }
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageManager::Conda => write!(f, "conda"),
            PackageManager::Mamba => write!(f, "mamba"),
            PackageManager::Micromamba => write!(f, "micromamba"),
            PackageManager::None => write!(f, "none"),
        }
    }
}

/// Detector with priority-based auto-detection
pub struct PackageManagerDetector {
    detected: Option<PackageManager>,
    detection_order: Vec<PackageManager>,
}

impl PackageManagerDetector {
    /// Default: conda → mamba → micromamba
    pub fn new() -> Self {
        Self {
            detected: None,
            detection_order: vec![
                PackageManager::Conda,
                PackageManager::Mamba,
                PackageManager::Micromamba,
            ],
        }
    }

    /// Create detector with custom priority order
    pub fn with_order(order: Vec<PackageManager>) -> Self {
        Self {
            detected: None,
            detection_order: order,
        }
    }

    /// Detect available PM with priority
    pub fn detect(&mut self) -> Result<PackageManager> {
        if let Some(pm) = self.detected {
            return Ok(pm);
        }

        for pm in &self.detection_order {
            if self.check_available(pm) {
                info!("✓ Detected package manager: {}", pm);
                self.detected = Some(*pm);
                return Ok(*pm);
            }
        }

        warn!("⚠ No package manager found (conda/mamba/micromamba)");
        self.detected = Some(PackageManager::None);
        Ok(PackageManager::None)
    }

    /// Check if PM is available and functional
    fn check_available(&self, pm: &PackageManager) -> bool {
        let cmd = pm.command();

        // Check in PATH
        match which(cmd) {
            Ok(_) => {
                // Verify it works
                if let Ok(output) = Command::new(cmd).arg("--version").output() {
                    if output.status.success() {
                        debug!("{} is available", cmd);
                        return true;
                    }
                }
            }
            Err(_) => {
                debug!("{} not found in PATH", cmd);
            }
        }
        false
    }

    /// Get detected PM
    pub fn get(&self) -> Option<PackageManager> {
        self.detected
    }

    /// Get run command for environment
    pub fn get_run_command(&self, env_name: &str) -> String {
        match self.get() {
            Some(pm) => pm.run_syntax(env_name),
            None => env_name.to_string(),
        }
    }

    /// Detect with env var override
    pub fn detect_with_env_override(&mut self) -> Result<PackageManager> {
        // Check ENVA_PACKAGE_MANAGER env var
        if let Ok(env_pm) = std::env::var("ENVA_PACKAGE_MANAGER") {
            match env_pm.to_lowercase().as_str() {
                "conda" => {
                    info!("ENVA_PACKAGE_MANAGER=conda, forcing conda");
                    return self.detect_specific(PackageManager::Conda);
                }
                "mamba" => {
                    info!("ENVA_PACKAGE_MANAGER=mamba, forcing mamba");
                    return self.detect_specific(PackageManager::Mamba);
                }
                "micromamba" => {
                    info!("ENVA_PACKAGE_MANAGER=micromamba, forcing micromamba");
                    return self.detect_specific(PackageManager::Micromamba);
                }
                _ => {
                    warn!("Unknown ENVA_PACKAGE_MANAGER value: {}", env_pm);
                }
            }
        }
        self.detect()
    }

    /// Force detection of specific PM
    pub fn detect_specific(&mut self, pm: PackageManager) -> Result<PackageManager> {
        if self.check_available(&pm) {
            info!("Using package manager: {}", pm);
            self.detected = Some(pm);
            return Ok(pm);
        }

        warn!("Package manager '{}' not found, falling back to auto-detection", pm);
        self.detect()
    }
}

/// Global detector instance
static GLOBAL_DETECTOR: std::sync::OnceLock<std::sync::Mutex<PackageManagerDetector>> =
    std::sync::OnceLock::new();

/// Get global detector
pub fn get_global_detector() -> &'static std::sync::Mutex<PackageManagerDetector> {
    GLOBAL_DETECTOR.get_or_init(|| std::sync::Mutex::new(PackageManagerDetector::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pm_command() {
        assert_eq!(PackageManager::Conda.command(), "conda");
        assert_eq!(PackageManager::Mamba.command(), "mamba");
        assert_eq!(PackageManager::Micromamba.command(), "micromamba");
        assert_eq!(PackageManager::None.command(), "");
    }

    #[test]
    fn test_run_syntax() {
        assert_eq!(
            PackageManager::Conda.run_syntax("test_env"),
            "conda run -n test_env"
        );
        assert_eq!(
            PackageManager::Mamba.run_syntax("test_env"),
            "mamba run -n test_env"
        );
        assert_eq!(
            PackageManager::Micromamba.run_syntax("test_env"),
            "micromamba run -n test_env"
        );
        assert_eq!(PackageManager::None.run_syntax("test_env"), "test_env");
    }

    #[test]
    fn test_pm_display() {
        assert_eq!(format!("{}", PackageManager::Conda), "conda");
        assert_eq!(format!("{}", PackageManager::Mamba), "mamba");
        assert_eq!(format!("{}", PackageManager::Micromamba), "micromamba");
        assert_eq!(format!("{}", PackageManager::None), "none");
    }

    #[test]
    fn test_detector_new() {
        let detector = PackageManagerDetector::new();
        assert!(detector.detected.is_none());
        assert_eq!(detector.detection_order.len(), 3);
        assert_eq!(detector.detection_order[0], PackageManager::Conda);
        assert_eq!(detector.detection_order[1], PackageManager::Mamba);
        assert_eq!(detector.detection_order[2], PackageManager::Micromamba);
    }

    #[test]
    fn test_detector_with_order() {
        let custom_order = vec![
            PackageManager::Micromamba,
            PackageManager::Mamba,
            PackageManager::Conda,
        ];
        let detector = PackageManagerDetector::with_order(custom_order.clone());
        assert_eq!(detector.detection_order, custom_order);
    }

    #[test]
    fn test_get_run_command_no_detection() {
        let detector = PackageManagerDetector::new();
        // Without detection, should return env_name as-is
        assert_eq!(detector.get_run_command("test_env"), "test_env");
    }

    #[test]
    fn test_get_run_command_with_detection() {
        let mut detector = PackageManagerDetector::new();
        // Simulate detection by setting detected field directly
        detector.detected = Some(PackageManager::Mamba);
        assert_eq!(
            detector.get_run_command("test_env"),
            "mamba run -n test_env"
        );
    }
}
