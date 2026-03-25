//! Compatibility package-manager detection for enva.
//!
//! enva is rattler-first. This module only discovers secondary package managers
//! (`micromamba`, `mamba`, `conda`) for compatibility scenarios such as
//! environment discovery, adoption, and explicit fallback flows.

use crate::error::Result;
use clap::ValueEnum;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tracing::{debug, info, warn};
use which::which;

/// Package manager type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum PackageManager {
    Conda,
    Mamba,
    Micromamba,
    #[value(skip)]
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

    /// Parse package manager name from string
    pub fn from_name(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "conda" => Some(PackageManager::Conda),
            "mamba" => Some(PackageManager::Mamba),
            "micromamba" => Some(PackageManager::Micromamba),
            _ => None,
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

fn availability_cache() -> &'static Mutex<HashMap<PackageManager, bool>> {
    static AVAILABILITY_CACHE: OnceLock<Mutex<HashMap<PackageManager, bool>>> = OnceLock::new();
    AVAILABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Detector for compatibility package managers
pub struct PackageManagerDetector {
    detected: Option<PackageManager>,
    detection_order: Vec<PackageManager>,
}

impl PackageManagerDetector {
    /// Default compatibility preference: micromamba → mamba → conda
    pub fn new() -> Self {
        Self {
            detected: None,
            detection_order: vec![
                PackageManager::Micromamba,
                PackageManager::Mamba,
                PackageManager::Conda,
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

    fn prioritized_order(&self, preferred: Option<PackageManager>) -> Vec<PackageManager> {
        let mut order = self.detection_order.clone();
        if let Some(pm) = preferred {
            if let Some(index) = order.iter().position(|candidate| *candidate == pm) {
                order.remove(index);
                order.insert(0, pm);
            }
        }
        order
    }

    fn preferred_manager_from_env() -> Option<PackageManager> {
        std::env::var("ENVA_PACKAGE_MANAGER")
            .ok()
            .as_deref()
            .and_then(PackageManager::from_name)
    }

    /// List available package managers using the detector's default order
    pub fn available_managers(&self) -> Vec<PackageManager> {
        self.available_managers_with_preference(None)
    }

    /// List available package managers, moving the preferred manager to the front when present
    pub fn available_managers_with_preference(
        &self,
        preferred: Option<PackageManager>,
    ) -> Vec<PackageManager> {
        self.prioritized_order(preferred)
            .into_iter()
            .filter(|pm| self.check_available(pm))
            .collect()
    }

    /// List available compatibility package managers while honoring ENVA_PACKAGE_MANAGER as a first-choice hint
    pub fn available_managers_with_env_override(&self) -> Vec<PackageManager> {
        self.available_managers_with_preference(Self::preferred_manager_from_env())
    }

    /// Detect available PM with priority
    pub fn detect(&mut self) -> Result<PackageManager> {
        if let Some(pm) = self.detected {
            return Ok(pm);
        }

        if let Some(pm) = self.available_managers().first().copied() {
            info!("✓ Detected package manager: {}", pm);
            self.detected = Some(pm);
            return Ok(pm);
        }

        warn!("⚠ No package manager found (conda/mamba/micromamba)");
        self.detected = Some(PackageManager::None);
        Ok(PackageManager::None)
    }

    /// Check if PM is available and functional
    fn check_available(&self, pm: &PackageManager) -> bool {
        if let Ok(cache) = availability_cache().lock() {
            if let Some(available) = cache.get(pm) {
                return *available;
            }
        }

        let cmd = pm.command();
        let available = match which(cmd) {
            Ok(path) => match Command::new(&path).arg("--version").output() {
                Ok(output) if output.status.success() => {
                    debug!("{} is available at {}", cmd, path.display());
                    true
                }
                Ok(output) => {
                    debug!(
                        "{} failed health check with status {:?}",
                        cmd,
                        output.status.code()
                    );
                    false
                }
                Err(error) => {
                    debug!("{} failed health check: {}", cmd, error);
                    false
                }
            },
            Err(_) => {
                debug!("{} not found in PATH", cmd);
                false
            }
        };

        if let Ok(mut cache) = availability_cache().lock() {
            cache.insert(*pm, available);
        }

        available
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

        warn!(
            "Package manager '{}' not found, falling back to auto-detection",
            pm
        );
        self.detect()
    }
}

/// Global detector instance
static GLOBAL_DETECTOR: OnceLock<Mutex<PackageManagerDetector>> = OnceLock::new();

/// Get global detector
pub fn get_global_detector() -> &'static Mutex<PackageManagerDetector> {
    GLOBAL_DETECTOR.get_or_init(|| Mutex::new(PackageManagerDetector::new()))
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
        assert_eq!(detector.detection_order[0], PackageManager::Micromamba);
        assert_eq!(detector.detection_order[1], PackageManager::Mamba);
        assert_eq!(detector.detection_order[2], PackageManager::Conda);
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
        assert_eq!(detector.get_run_command("test_env"), "test_env");
    }

    #[test]
    fn test_get_run_command_with_detection() {
        let mut detector = PackageManagerDetector::new();
        detector.detected = Some(PackageManager::Mamba);
        assert_eq!(
            detector.get_run_command("test_env"),
            "mamba run -n test_env"
        );
    }

    #[test]
    fn test_prioritized_order_moves_preference_to_front() {
        let detector = PackageManagerDetector::new();
        assert_eq!(
            detector.prioritized_order(Some(PackageManager::Conda)),
            vec![
                PackageManager::Conda,
                PackageManager::Micromamba,
                PackageManager::Mamba,
            ]
        );
    }

    #[test]
    fn test_from_name() {
        assert_eq!(
            PackageManager::from_name("conda"),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            PackageManager::from_name("MAMBA"),
            Some(PackageManager::Mamba)
        );
        assert_eq!(
            PackageManager::from_name("micromamba"),
            Some(PackageManager::Micromamba)
        );
        assert_eq!(PackageManager::from_name("unknown"), None);
    }
}
