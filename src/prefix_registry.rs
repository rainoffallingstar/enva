use crate::error::Result;
use crate::micromamba::{CondaEnvironment, MicromambaManager};
use crate::ownership::read_ownership_record;
use crate::package_manager::{PackageManager, PackageManagerDetector};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentSource {
    Rattler,
    PackageManager(PackageManager),
}

impl EnvironmentSource {
    pub fn label(&self) -> String {
        match self {
            Self::Rattler => "rattler".to_string(),
            Self::PackageManager(package_manager) => package_manager.to_string(),
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("rattler") {
            return Some(Self::Rattler);
        }

        PackageManager::from_name(value).map(Self::PackageManager)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentOwner {
    Rattler,
    External,
}

impl EnvironmentOwner {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rattler => "rattler",
            Self::External => "external",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEnvironment {
    pub name: String,
    pub prefix: PathBuf,
    pub is_active: bool,
    pub source: EnvironmentSource,
    pub owner: EnvironmentOwner,
    pub adopted_from: Option<EnvironmentSource>,
}

impl DiscoveredEnvironment {
    pub fn from_conda_environment(
        package_manager: PackageManager,
        environment: CondaEnvironment,
    ) -> Result<Self> {
        let source = EnvironmentSource::PackageManager(package_manager);
        let ownership_record = read_ownership_record(PathBuf::from(&environment.prefix).as_path())?;
        let owner = ownership_record
            .as_ref()
            .filter(|record| record.is_rattler_owned())
            .map(|_| EnvironmentOwner::Rattler)
            .unwrap_or(EnvironmentOwner::External);
        let adopted_from = ownership_record
            .as_ref()
            .and_then(|record| record.adopted_from.as_deref())
            .and_then(EnvironmentSource::from_label)
            .or_else(|| match owner {
                EnvironmentOwner::Rattler => Some(source.clone()),
                EnvironmentOwner::External => None,
            });

        Ok(Self {
            name: environment.name,
            prefix: PathBuf::from(environment.prefix),
            is_active: environment.is_active,
            source,
            owner,
            adopted_from,
        })
    }

    pub fn rattler_managed(&self) -> bool {
        self.owner == EnvironmentOwner::Rattler
    }

    pub fn owner_label(&self) -> &'static str {
        self.owner.label()
    }

    pub fn adopted_from_label(&self) -> Option<String> {
        self.adopted_from.as_ref().map(EnvironmentSource::label)
    }
}

pub async fn discover_cli_environments() -> Result<Vec<DiscoveredEnvironment>> {
    let detector = PackageManagerDetector::new();
    let package_managers = detector.available_managers_with_env_override();
    let mut environments = Vec::new();

    for package_manager in package_managers {
        let manager =
            match MicromambaManager::new_runtime_with_package_manager(package_manager).await {
                Ok(manager) => manager,
                Err(error) => {
                    warn!(
                    "Skipping {} environment discovery because manager initialization failed: {}",
                    package_manager, error
                );
                    continue;
                }
            };

        let discovered = match manager.get_all_conda_environments().await {
            Ok(environments_for_manager) => environments_for_manager,
            Err(error) => {
                warn!(
                    "Skipping {} environment discovery because env listing failed: {}",
                    package_manager, error
                );
                continue;
            }
        };

        for environment in discovered {
            environments.push(DiscoveredEnvironment::from_conda_environment(
                package_manager,
                environment,
            )?);
        }
    }

    Ok(dedupe_discovered_environments(environments))
}

fn source_priority(source: &EnvironmentSource) -> u8 {
    match source {
        EnvironmentSource::Rattler => 0,
        EnvironmentSource::PackageManager(PackageManager::Micromamba) => 1,
        EnvironmentSource::PackageManager(PackageManager::Mamba) => 2,
        EnvironmentSource::PackageManager(PackageManager::Conda) => 3,
        EnvironmentSource::PackageManager(PackageManager::None) => 4,
    }
}

fn environment_priority(environment: &DiscoveredEnvironment) -> (u8, u8, String) {
    (
        if environment.rattler_managed() { 0 } else { 1 },
        source_priority(&environment.source),
        environment.prefix.display().to_string(),
    )
}

fn canonical_environment_identity(prefix: &Path) -> PathBuf {
    fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf())
}

pub fn dedupe_discovered_environments(
    environments: impl IntoIterator<Item = DiscoveredEnvironment>,
) -> Vec<DiscoveredEnvironment> {
    let mut deduped = BTreeMap::new();

    for environment in environments {
        if environment.prefix.as_os_str().is_empty() {
            continue;
        }

        let key = canonical_environment_identity(&environment.prefix);
        match deduped.get(&key) {
            Some(existing)
                if environment_priority(existing) <= environment_priority(&environment) => {}
            _ => {
                deduped.insert(key, environment);
            }
        }
    }

    deduped.into_values().collect()
}

pub fn merge_discovered_environments(
    primary: Vec<DiscoveredEnvironment>,
    secondary: Vec<DiscoveredEnvironment>,
) -> Vec<DiscoveredEnvironment> {
    dedupe_discovered_environments(primary.into_iter().chain(secondary))
}

#[cfg(test)]
mod tests {
    use super::{
        dedupe_discovered_environments, merge_discovered_environments, DiscoveredEnvironment,
        EnvironmentOwner, EnvironmentSource,
    };
    use crate::package_manager::PackageManager;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn environment_at(
        name: &str,
        prefix: PathBuf,
        source: EnvironmentSource,
        owner: EnvironmentOwner,
        adopted_from: Option<EnvironmentSource>,
    ) -> DiscoveredEnvironment {
        DiscoveredEnvironment {
            name: name.to_string(),
            prefix,
            is_active: false,
            source,
            owner,
            adopted_from,
        }
    }

    fn environment(
        name: &str,
        prefix: &str,
        source: EnvironmentSource,
        owner: EnvironmentOwner,
        adopted_from: Option<EnvironmentSource>,
    ) -> DiscoveredEnvironment {
        environment_at(name, PathBuf::from(prefix), source, owner, adopted_from)
    }

    #[test]
    fn dedupe_discovered_environments_keeps_rattler_owned_prefix_owner() {
        let environments = dedupe_discovered_environments(vec![
            environment(
                "demo",
                "/tmp/demo",
                EnvironmentSource::PackageManager(PackageManager::Micromamba),
                EnvironmentOwner::External,
                None,
            ),
            environment(
                "demo",
                "/tmp/demo",
                EnvironmentSource::PackageManager(PackageManager::Conda),
                EnvironmentOwner::Rattler,
                Some(EnvironmentSource::PackageManager(PackageManager::Conda)),
            ),
        ]);

        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].owner, EnvironmentOwner::Rattler);
    }

    #[cfg(unix)]
    #[test]
    fn dedupe_discovered_environments_collapses_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let temporary_directory = tempdir().unwrap();
        let real_prefix = temporary_directory.path().join("real-prefix");
        let alias_prefix = temporary_directory.path().join("alias-prefix");
        fs::create_dir_all(&real_prefix).unwrap();
        symlink(&real_prefix, &alias_prefix).unwrap();

        let environments = dedupe_discovered_environments(vec![
            environment_at(
                "demo",
                alias_prefix,
                EnvironmentSource::PackageManager(PackageManager::Conda),
                EnvironmentOwner::External,
                None,
            ),
            environment_at(
                "demo",
                real_prefix.clone(),
                EnvironmentSource::Rattler,
                EnvironmentOwner::Rattler,
                None,
            ),
        ]);

        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].prefix, real_prefix);
        assert_eq!(environments[0].owner, EnvironmentOwner::Rattler);
    }

    #[test]
    fn dedupe_discovered_environments_preserves_distinct_same_name_prefixes() {
        let temporary_directory = tempdir().unwrap();
        let first_prefix = temporary_directory.path().join("first").join("demo");
        let second_prefix = temporary_directory.path().join("second").join("demo");
        fs::create_dir_all(&first_prefix).unwrap();
        fs::create_dir_all(&second_prefix).unwrap();

        let environments = dedupe_discovered_environments(vec![
            environment_at(
                "demo",
                first_prefix,
                EnvironmentSource::Rattler,
                EnvironmentOwner::Rattler,
                None,
            ),
            environment_at(
                "demo",
                second_prefix,
                EnvironmentSource::PackageManager(PackageManager::Conda),
                EnvironmentOwner::External,
                None,
            ),
        ]);

        assert_eq!(environments.len(), 2);
    }

    #[test]
    fn merge_discovered_environments_prefers_primary_entries() {
        let merged = merge_discovered_environments(
            vec![environment(
                "demo",
                "/tmp/demo",
                EnvironmentSource::Rattler,
                EnvironmentOwner::Rattler,
                None,
            )],
            vec![environment(
                "demo",
                "/tmp/demo",
                EnvironmentSource::PackageManager(PackageManager::Micromamba),
                EnvironmentOwner::External,
                None,
            )],
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, EnvironmentSource::Rattler);
        assert_eq!(merged[0].owner, EnvironmentOwner::Rattler);
    }
}
