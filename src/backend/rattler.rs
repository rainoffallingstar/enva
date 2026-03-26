use super::{BackendKind, EnvironmentBackend, EnvironmentTarget, OutputMode, RunRequest};
use crate::error::{EnvError, Result};
use crate::micromamba::{CondaEnvironment, MicromambaManager, ValidationDetails, ValidationResult};
use crate::ownership::{
    ownership_record_path, read_ownership_record, write_rattler_ownership_record,
};
use crate::package_manager::{PackageManager, PackageManagerDetector};
use crate::prefix_registry::{
    discover_cli_environments, merge_discovered_environments, DiscoveredEnvironment,
    EnvironmentOwner, EnvironmentSource,
};
use async_trait::async_trait;
use indicatif::{ProgressBar, ProgressStyle};
use rattler::install::Installer;
use rattler::package_cache::PackageCache;
use rattler_conda_types::{
    Channel, ChannelConfig, EnvironmentYaml, MatchSpec, Platform, PrefixRecord, RepoDataRecord,
};
use rattler_repodata_gateway::{Gateway, RepoData};
use rattler_solve::{resolvo::Solver as RattlerSolver, ChannelPriority, SolverImpl, SolverTask};
use rattler_virtual_packages::{VirtualPackage, VirtualPackageOverrides};
use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs as async_fs;
use tokio::process::Command as AsyncCommand;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RattlerBackend {
    root_prefixes: Vec<PathBuf>,
    creation_lock: Arc<Mutex<()>>,
}

impl Default for RattlerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RattlerBackend {
    pub fn new() -> Self {
        Self {
            root_prefixes: Self::detect_root_prefixes(),
            creation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_root_prefixes(root_prefixes: Vec<PathBuf>) -> Self {
        Self {
            root_prefixes: Self::dedupe_paths(root_prefixes),
            creation_lock: Arc::new(Mutex::new(())),
        }
    }

    fn detect_root_prefixes() -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        for variable in [
            "ENVA_RATTLER_ROOT_PREFIX",
            "RATTLER_ROOT_PREFIX",
            "MAMBA_ROOT_PREFIX",
        ] {
            if let Some(value) = std::env::var_os(variable) {
                candidates.extend(std::env::split_paths(&value));
            }
        }

        if let Some(conda_prefix) = std::env::var_os("CONDA_PREFIX").map(PathBuf::from) {
            if let Some(parent) = conda_prefix.parent() {
                if parent.file_name().and_then(|name| name.to_str()) == Some("envs") {
                    if let Some(root_prefix) = parent.parent() {
                        candidates.push(root_prefix.to_path_buf());
                    }
                } else {
                    candidates.push(conda_prefix);
                }
            } else {
                candidates.push(conda_prefix);
            }
        }

        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".local/share/rattler"));
            candidates.push(home.join(".local/share/mamba"));
            candidates.push(home.join(".conda"));
        }

        Self::dedupe_paths(candidates)
    }

    fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut unique = Vec::new();

        for path in paths
            .into_iter()
            .filter(|path| !path.as_os_str().is_empty())
        {
            if seen.insert(path.clone()) {
                unique.push(path);
            }
        }

        unique
    }

    fn default_root_prefix() -> PathBuf {
        dirs::home_dir()
            .map(|home| home.join(".local/share/rattler"))
            .unwrap_or_else(|| PathBuf::from("/tmp/rattler"))
    }

    fn preferred_root_prefix(&self) -> PathBuf {
        self.root_prefixes
            .iter()
            .find(|root| root.exists())
            .cloned()
            .or_else(|| self.root_prefixes.first().cloned())
            .unwrap_or_else(Self::default_root_prefix)
    }

    fn target_prefix_for_env_name(&self, env_name: &str) -> Result<PathBuf> {
        if env_name == "base" {
            return Err(EnvError::Validation(
                "rattler backend does not support creating the base environment".to_string(),
            ));
        }

        Ok(self.preferred_root_prefix().join("envs").join(env_name))
    }

    fn parse_environment_yaml(yaml_file: &Path) -> Result<EnvironmentYaml> {
        EnvironmentYaml::from_path(yaml_file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                EnvError::Validation(format!("Invalid YAML syntax: {}", error))
            } else {
                EnvError::FileOperation(format!("Failed to read YAML file: {}", error))
            }
        })
    }

    fn environment_issues(environment_yaml: &EnvironmentYaml) -> Vec<String> {
        let mut issues = Vec::new();

        if environment_yaml.match_specs().next().is_none() {
            issues.push("Missing required 'dependencies' section".to_string());
        }

        if environment_yaml.channels.is_empty() {
            issues.push(
                "No channels defined; rattler backend requires explicit channels".to_string(),
            );
        }

        if let Some(pip_specs) = environment_yaml
            .pip_specs()
            .filter(|specs| !specs.is_empty())
        {
            issues.push(format!(
                "pip subsection is not supported yet by rattler backend ({} pip specs)",
                pip_specs.len()
            ));
        }

        issues
    }

    fn conda_specs(environment_yaml: &EnvironmentYaml) -> Vec<MatchSpec> {
        environment_yaml.match_specs().cloned().collect()
    }

    fn default_channels() -> Vec<String> {
        vec!["conda-forge".to_string(), "bioconda".to_string()]
    }

    fn default_channel_priority() -> ChannelPriority {
        ChannelPriority::Disabled
    }

    fn summary_spinner(message: impl Into<String>) -> Result<ProgressBar> {
        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .map_err(|error| {
                EnvError::Template(format!("Failed to create progress spinner: {}", error))
            })?
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
        pb.set_style(style);
        pb.set_message(message.into());
        pb.enable_steady_tick(Duration::from_millis(120));
        Ok(pb)
    }

    fn resolve_channel_config(yaml_file: &Path) -> ChannelConfig {
        let root_dir = yaml_file
            .parent()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        ChannelConfig::default_with_root_dir(root_dir)
    }

    fn resolve_channel_names(
        channel_config: &ChannelConfig,
        channel_names: Vec<String>,
    ) -> Result<Vec<Channel>> {
        channel_names
            .into_iter()
            .map(|channel| {
                let channel_label = channel.to_string();
                Channel::from_str(channel.as_str(), channel_config).map_err(|error| {
                    EnvError::Validation(format!(
                        "Failed to parse channel '{}': {}",
                        channel_label, error
                    ))
                })
            })
            .collect()
    }

    fn resolve_channels(
        yaml_file: &Path,
        environment_yaml: &EnvironmentYaml,
    ) -> Result<Vec<Channel>> {
        let channel_config = Self::resolve_channel_config(yaml_file);
        Self::resolve_channel_names(&channel_config, Self::extract_string_list(environment_yaml))
    }

    fn resolve_channels_for_prefix(
        prefix: &Path,
        channel_names: Vec<String>,
    ) -> Result<Vec<Channel>> {
        let channel_config = ChannelConfig::default_with_root_dir(prefix.to_path_buf());
        Self::resolve_channel_names(&channel_config, channel_names)
    }

    fn exact_match_spec_for_record(record: &PrefixRecord) -> String {
        format!(
            "{} =={} {}",
            record.repodata_record.package_record.name.as_normalized(),
            record.repodata_record.package_record.version,
            record.repodata_record.package_record.build
        )
    }

    fn push_unique_string(values: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
        if !value.trim().is_empty() && seen.insert(value.clone()) {
            values.push(value);
        }
    }

    fn requested_spec_strings_from_prefix_records(installed: &[PrefixRecord]) -> Vec<String> {
        let mut specs = Vec::new();
        let mut seen = HashSet::new();

        for record in installed {
            #[allow(deprecated)]
            if let Some(requested_spec) = record.requested_spec.clone() {
                Self::push_unique_string(&mut specs, &mut seen, requested_spec);
            }

            for requested_spec in &record.requested_specs {
                Self::push_unique_string(&mut specs, &mut seen, requested_spec.clone());
            }
        }

        if specs.is_empty() {
            for record in installed {
                Self::push_unique_string(
                    &mut specs,
                    &mut seen,
                    Self::exact_match_spec_for_record(record),
                );
            }
        }

        specs
    }

    fn merge_requested_install_specs(
        existing_specs: Vec<String>,
        packages: &[String],
    ) -> Vec<String> {
        let mut merged = Vec::new();
        let mut seen = HashSet::new();

        for spec in existing_specs {
            Self::push_unique_string(&mut merged, &mut seen, spec);
        }

        for package in packages {
            Self::push_unique_string(&mut merged, &mut seen, package.clone());
        }

        merged
    }

    fn channel_hints_from_spec_strings(specs: &[String]) -> Vec<String> {
        let mut channels = Vec::new();
        let mut seen = HashSet::new();

        for spec in specs {
            if let Some((channel, _)) = spec.split_once("::") {
                Self::push_unique_string(&mut channels, &mut seen, channel.trim().to_string());
            }
        }

        channels
    }

    fn channel_hints_from_prefix_records(installed: &[PrefixRecord]) -> Vec<String> {
        let mut channels = Vec::new();
        let mut seen = HashSet::new();

        for record in installed {
            if let Some(channel) = record.repodata_record.channel.clone() {
                Self::push_unique_string(&mut channels, &mut seen, channel);
            }
        }

        channels
    }

    fn install_channel_hints(
        installed: &[PrefixRecord],
        requested_specs: &[String],
    ) -> Vec<String> {
        let mut channels = Vec::new();
        let mut seen = HashSet::new();

        for channel in Self::channel_hints_from_prefix_records(installed) {
            Self::push_unique_string(&mut channels, &mut seen, channel);
        }

        for channel in Self::channel_hints_from_spec_strings(requested_specs) {
            Self::push_unique_string(&mut channels, &mut seen, channel);
        }

        if channels.is_empty() {
            return Self::default_channels();
        }

        channels
    }

    fn collect_installed_prefix_records(prefix: &Path) -> Result<Vec<PrefixRecord>> {
        let conda_meta_path = prefix.join("conda-meta");

        if !conda_meta_path.exists() {
            return Ok(Vec::new());
        }

        let ownership_marker = ownership_record_path(prefix);
        let mut json_paths: Vec<_> = fs::read_dir(&conda_meta_path)
            .map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to read {}: {}",
                    conda_meta_path.display(),
                    error
                ))
            })?
            .filter_map(|entry| {
                entry.ok().and_then(|entry| {
                    let path = entry.path();
                    let is_json = entry.file_type().ok()?.is_file()
                        && path.extension().and_then(|ext| ext.to_str()) == Some("json");
                    if is_json && path != ownership_marker {
                        Some(path)
                    } else {
                        None
                    }
                })
            })
            .collect();

        json_paths.sort();

        json_paths
            .into_iter()
            .map(|record_path| {
                PrefixRecord::from_path(&record_path).map_err(|error| {
                    EnvError::FileOperation(format!(
                        "Failed to parse prefix record {}: {}",
                        record_path.display(),
                        error
                    ))
                })
            })
            .collect()
    }

    fn stash_ownership_marker(prefix: &Path) -> Result<Option<String>> {
        let marker_path = ownership_record_path(prefix);
        if !marker_path.is_file() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&marker_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to read ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })?;

        fs::remove_file(&marker_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to temporarily remove ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })?;

        Ok(Some(contents))
    }

    fn restore_ownership_marker(prefix: &Path, contents: Option<String>) -> Result<()> {
        let Some(contents) = contents else {
            return Ok(());
        };

        let marker_path = ownership_record_path(prefix);
        fs::write(&marker_path, contents).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to restore ownership marker {}: {}",
                marker_path.display(),
                error
            ))
        })
    }

    fn parse_match_specs(spec_strings: &[String]) -> Result<Vec<MatchSpec>> {
        spec_strings
            .iter()
            .map(|spec| {
                <MatchSpec as FromStr>::from_str(spec).map_err(|error| {
                    EnvError::Validation(format!(
                        "Failed to parse package spec '{}': {}",
                        spec, error
                    ))
                })
            })
            .collect()
    }

    fn detect_virtual_packages() -> Result<Vec<rattler_conda_types::GenericVirtualPackage>> {
        let overrides = VirtualPackageOverrides::from_env();
        VirtualPackage::detect(&overrides)
            .map(|packages| packages.into_iter().map(Into::into).collect())
            .map_err(|error| {
                EnvError::Environment(format!(
                    "Failed to detect virtual packages for rattler solve: {}",
                    error
                ))
            })
    }

    async fn solve_environment(
        &self,
        yaml_file: &Path,
        environment_yaml: &EnvironmentYaml,
    ) -> Result<(Vec<MatchSpec>, Vec<RepoDataRecord>)> {
        let specs = Self::conda_specs(environment_yaml);
        let channels = Self::resolve_channels(yaml_file, environment_yaml)?;
        let virtual_packages = Self::detect_virtual_packages()?;
        let platforms = [Platform::current(), Platform::NoArch];

        let cache_root = Self::cache_root_dir()?;
        let repo_data_sets: Vec<RepoData> = Gateway::builder()
            .with_cache_dir(cache_root.clone())
            .with_package_cache(PackageCache::new(Self::package_cache_dir(&cache_root)))
            .finish()
            .query(channels, platforms, specs.clone())
            .recursive(true)
            .execute()
            .await
            .map_err(|error| {
                EnvError::Execution(format!("Failed to fetch repodata for solve: {}", error))
            })?;

        if repo_data_sets.iter().all(RepoData::is_empty) {
            return Err(EnvError::Execution(
                "No package metadata was returned for the requested channels and specs".to_string(),
            ));
        }

        let mut solver = RattlerSolver::default();
        let solved = solver
            .solve(SolverTask {
                specs: specs.clone(),
                virtual_packages,
                channel_priority: Self::default_channel_priority(),
                ..SolverTask::from_iter(repo_data_sets.iter())
            })
            .map_err(|error| {
                EnvError::Execution(format!("Failed to solve environment: {}", error))
            })?;

        Ok((specs, solved.records))
    }

    fn extract_string_list(environment_yaml: &EnvironmentYaml) -> Vec<String> {
        environment_yaml
            .channels
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn is_environment_prefix(path: &Path) -> bool {
        path.join("conda-meta").is_dir()
    }

    fn environment_name_for_prefix(&self, prefix: &Path) -> String {
        if self.root_prefixes.iter().any(|root| root == prefix) {
            return "base".to_string();
        }

        prefix
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    fn owned_environment_records(&self) -> Result<Vec<DiscoveredEnvironment>> {
        let active_prefix = std::env::var("CONDA_PREFIX").ok();

        Ok(self
            .list_environment_prefixes()?
            .into_iter()
            .map(|prefix| {
                let ownership_record = read_ownership_record(&prefix).ok().flatten();
                let adopted_from = ownership_record
                    .as_ref()
                    .and_then(|record| record.adopted_from.as_deref())
                    .and_then(EnvironmentSource::from_label);
                let owner = ownership_record
                    .as_ref()
                    .filter(|record| record.is_rattler_owned())
                    .map(|_| EnvironmentOwner::Rattler)
                    .unwrap_or(EnvironmentOwner::External);
                let source = match (&owner, &adopted_from) {
                    (EnvironmentOwner::Rattler, Some(source)) => source.clone(),
                    (EnvironmentOwner::Rattler, None) => EnvironmentSource::Rattler,
                    (EnvironmentOwner::External, _) => {
                        EnvironmentSource::PackageManager(PackageManager::None)
                    }
                };

                DiscoveredEnvironment {
                    name: self.environment_name_for_prefix(&prefix),
                    is_active: active_prefix
                        .as_deref()
                        .map(|active| Path::new(active) == prefix)
                        .unwrap_or(false),
                    prefix,
                    source,
                    owner,
                    adopted_from,
                }
            })
            .collect())
    }

    async fn accessible_environment_records(&self) -> Result<Vec<DiscoveredEnvironment>> {
        let owned = self.owned_environment_records()?;
        let external = discover_cli_environments().await?;
        Ok(merge_discovered_environments(owned, external))
    }

    async fn remove_foreign_environment(
        &self,
        environment: &DiscoveredEnvironment,
        output_mode: OutputMode,
    ) -> Result<()> {
        match environment.source {
            EnvironmentSource::PackageManager(PackageManager::None) => {
                let manager = self.helper_manager_for_environment(environment).await?;
                manager
                    .remove_environment_by_prefix_with_output(&environment.prefix, output_mode)
                    .await
            }
            EnvironmentSource::PackageManager(package_manager) => {
                let manager =
                    MicromambaManager::new_runtime_with_package_manager(package_manager).await?;
                manager
                    .remove_environment_by_prefix_with_output(&environment.prefix, output_mode)
                    .await
            }
            EnvironmentSource::Rattler => Err(EnvError::Execution(format!(
                "Refusing to remove rattler-owned environment '{}' as a foreign conflict",
                environment.prefix.display()
            ))),
        }
    }

    async fn remove_conflicting_environment(
        &self,
        environment: &DiscoveredEnvironment,
        output_mode: OutputMode,
    ) -> Result<()> {
        if environment.rattler_managed() {
            let manager = self.helper_manager_for_environment(environment).await?;
            return manager
                .remove_environment_by_prefix_with_output(&environment.prefix, output_mode)
                .await;
        }

        self.remove_foreign_environment(environment, output_mode)
            .await
    }

    async fn remove_resolved_environment(
        &self,
        environment: DiscoveredEnvironment,
        display_name: &str,
        output_mode: OutputMode,
    ) -> Result<()> {
        let prefix = environment.prefix.clone();
        if self.root_prefixes.iter().any(|root| root == &prefix) {
            return Err(EnvError::Execution(
                "Refusing to remove the rattler base environment".to_string(),
            ));
        }

        if Self::helper_package_manager(&environment).is_some() {
            if matches!(output_mode, OutputMode::Stream | OutputMode::Summary) {
                println!(
                    "Removing adopted environment '{}' at {} via helper package manager",
                    display_name,
                    prefix.display()
                );
            }
            let manager = self.helper_manager_for_environment(&environment).await?;
            manager
                .remove_environment_by_prefix_with_output(&prefix, output_mode)
                .await?;
        } else {
            if matches!(output_mode, OutputMode::Stream | OutputMode::Summary) {
                println!(
                    "Removing rattler environment '{}' at {}",
                    display_name,
                    prefix.display()
                );
            }

            async_fs::remove_dir_all(&prefix).await.map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to remove rattler environment {}: {}",
                    prefix.display(),
                    error
                ))
            })?;

            if matches!(output_mode, OutputMode::Summary) {
                println!("✓ Environment {} removed", display_name);
            }
        }

        Ok(())
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

    fn source_label(source: &EnvironmentSource) -> String {
        match source {
            EnvironmentSource::Rattler => "rattler".to_string(),
            EnvironmentSource::PackageManager(package_manager) => package_manager.to_string(),
        }
    }

    fn management_priority(environment: &DiscoveredEnvironment) -> (u8, u8, String) {
        (
            if environment.rattler_managed() { 0 } else { 1 },
            Self::source_priority(&environment.source),
            environment.prefix.display().to_string(),
        )
    }

    fn prioritize_named_records(
        env_name: &str,
        records: Vec<DiscoveredEnvironment>,
    ) -> Vec<DiscoveredEnvironment> {
        let mut matches = records
            .into_iter()
            .filter(|environment| environment.name == env_name)
            .collect::<Vec<DiscoveredEnvironment>>();

        matches.sort_by(|left, right| {
            Self::management_priority(left)
                .cmp(&Self::management_priority(right))
                .then(left.prefix.cmp(&right.prefix))
        });

        matches
    }

    fn list_environment_prefixes(&self) -> Result<Vec<PathBuf>> {
        let mut prefixes = Vec::new();

        for root_prefix in &self.root_prefixes {
            if Self::is_environment_prefix(root_prefix) {
                prefixes.push(root_prefix.clone());
            }

            let envs_dir = root_prefix.join("envs");
            if !envs_dir.is_dir() {
                continue;
            }

            let entries = fs::read_dir(&envs_dir).map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to read rattler environments under {}: {}",
                    envs_dir.display(),
                    error
                ))
            })?;

            for entry in entries {
                let entry = entry.map_err(|error| {
                    EnvError::FileOperation(format!(
                        "Failed to inspect rattler environment entry in {}: {}",
                        envs_dir.display(),
                        error
                    ))
                })?;
                let path = entry.path();
                if path.is_dir() && Self::is_environment_prefix(&path) {
                    prefixes.push(path);
                }
            }
        }

        Ok(Self::dedupe_paths(prefixes))
    }

    fn helper_package_manager(environment: &DiscoveredEnvironment) -> Option<PackageManager> {
        environment
            .adopted_from
            .as_ref()
            .or(Some(&environment.source))
            .and_then(|source| match source {
                EnvironmentSource::PackageManager(package_manager)
                    if *package_manager != PackageManager::None =>
                {
                    Some(*package_manager)
                }
                _ => None,
            })
    }

    fn removable_conflict_manager_label(environment: &DiscoveredEnvironment) -> Option<String> {
        Self::helper_package_manager(environment).map(|package_manager| package_manager.to_string())
    }

    fn has_native_rattler_conflict(environment: &DiscoveredEnvironment) -> bool {
        environment.rattler_managed() && Self::helper_package_manager(environment).is_none()
    }

    async fn helper_manager_for_environment(
        &self,
        environment: &DiscoveredEnvironment,
    ) -> Result<MicromambaManager> {
        if let Some(package_manager) = Self::helper_package_manager(environment) {
            return MicromambaManager::new_runtime_with_package_manager(package_manager).await;
        }

        let detector = PackageManagerDetector::new();
        let package_manager = detector
            .available_managers_with_env_override()
            .into_iter()
            .next()
            .ok_or_else(|| {
                EnvError::Execution(
                    "No helper package manager is available for this rattler-managed operation"
                        .to_string(),
                )
            })?;

        MicromambaManager::new_runtime_with_package_manager(package_manager).await
    }

    async fn adopt_discovered_environment(
        &self,
        environment: &DiscoveredEnvironment,
        output_mode: OutputMode,
    ) -> Result<DiscoveredEnvironment> {
        if environment.rattler_managed() {
            return Ok(environment.clone());
        }

        let adopted_from = environment
            .adopted_from_label()
            .or_else(|| match &environment.source {
                EnvironmentSource::PackageManager(package_manager) => {
                    Some(package_manager.to_string())
                }
                EnvironmentSource::Rattler => None,
            });
        write_rattler_ownership_record(&environment.prefix, adopted_from.as_deref())?;

        if matches!(output_mode, OutputMode::Summary | OutputMode::Stream) {
            println!(
                "Adopted environment '{}' at {} into rattler ownership{}",
                environment.name,
                environment.prefix.display(),
                adopted_from
                    .as_ref()
                    .map(|source| format!(" (source: {})", source))
                    .unwrap_or_default()
            );
        }

        let mut adopted = environment.clone();
        adopted.owner = EnvironmentOwner::Rattler;
        adopted.adopted_from = adopted_from
            .as_deref()
            .and_then(EnvironmentSource::from_label);
        Ok(adopted)
    }

    async fn resolve_record_by_prefix(&self, prefix: &Path) -> Result<DiscoveredEnvironment> {
        let matches = self
            .accessible_environment_records()
            .await?
            .into_iter()
            .filter(|environment| environment.prefix == prefix)
            .collect::<Vec<DiscoveredEnvironment>>();

        match matches.as_slice() {
            [environment] => Ok(environment.clone()),
            [] if Self::is_environment_prefix(prefix) => {
                let active_prefix = std::env::var("CONDA_PREFIX").ok();
                let adopted_from = read_ownership_record(prefix)
                    .ok()
                    .flatten()
                    .and_then(|record| record.adopted_from)
                    .and_then(|source| EnvironmentSource::from_label(&source));
                Ok(DiscoveredEnvironment {
                    name: self.environment_name_for_prefix(prefix),
                    prefix: prefix.to_path_buf(),
                    is_active: active_prefix
                        .as_deref()
                        .map(|active| Path::new(active) == prefix)
                        .unwrap_or(false),
                    source: EnvironmentSource::PackageManager(PackageManager::None),
                    owner: if adopted_from.is_some() {
                        EnvironmentOwner::Rattler
                    } else {
                        EnvironmentOwner::External
                    },
                    adopted_from,
                })
            }
            [] => Err(EnvError::Execution(format!(
                "Environment prefix was not found in accessible environment prefixes: {}",
                prefix.display()
            ))),
            _ => Err(EnvError::Execution(format!(
                "Environment prefix matched multiple records: {}",
                prefix.display()
            ))),
        }
    }

    async fn resolve_unique_record_by_name(&self, env_name: &str) -> Result<DiscoveredEnvironment> {
        let matches =
            Self::prioritize_named_records(env_name, self.accessible_environment_records().await?);

        match matches.as_slice() {
            [] => Err(EnvError::Execution(format!(
                "Environment '{}' was not found in accessible environment prefixes",
                env_name
            ))),
            [environment] => Ok(environment.clone()),
            _ => Err(EnvError::Execution(format!(
                "Environment '{}' matched multiple accessible prefixes: {}. Use --prefix to disambiguate.",
                env_name,
                matches
                    .iter()
                    .map(|environment| environment.prefix.display().to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            ))),
        }
    }

    async fn ensure_adopted_environment(
        &self,
        target: &EnvironmentTarget,
        output_mode: OutputMode,
    ) -> Result<DiscoveredEnvironment> {
        let environment = match target {
            EnvironmentTarget::Name(env_name) => {
                self.resolve_unique_record_by_name(env_name).await?
            }
            EnvironmentTarget::Prefix(prefix) => self.resolve_record_by_prefix(prefix).await?,
        };

        if environment.rattler_managed() {
            return Ok(environment);
        }

        self.adopt_discovered_environment(&environment, output_mode)
            .await
    }

    fn default_local_cache_root() -> PathBuf {
        let tmp_root = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(std::env::temp_dir);
        let user = std::env::var("USER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string())
            .replace(std::path::is_separator, "_");

        tmp_root.join(format!("enva-rattler-cache-{}", user))
    }

    fn cache_root_dir() -> Result<PathBuf> {
        if let Some(value) = std::env::var_os("RATTLER_CACHE_DIR") {
            let path = PathBuf::from(value);
            if !path.as_os_str().is_empty() {
                return Ok(path);
            }
        }

        if std::env::var_os("XDG_CACHE_HOME").is_some() {
            return rattler::default_cache_dir().map_err(|error| {
                EnvError::Environment(format!(
                    "Failed to determine rattler cache directory: {}",
                    error
                ))
            });
        }

        Ok(Self::default_local_cache_root())
    }

    fn package_cache_dir(cache_root: &Path) -> PathBuf {
        cache_root.join("pkgs")
    }

    fn cache_directory_entries(cache_root: &Path) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("packages", cache_root.join("pkgs")),
            ("repodata", cache_root.join("repodata")),
            ("run-exports", cache_root.join("run-exports")),
        ]
    }

    async fn clear_cache_directory(path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        if path.is_dir() {
            async_fs::remove_dir_all(path).await.map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to remove rattler cache directory {}: {}",
                    path.display(),
                    error
                ))
            })?;
        } else {
            async_fs::remove_file(path).await.map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to remove rattler cache file {}: {}",
                    path.display(),
                    error
                ))
            })?;
        }

        Ok(())
    }

    async fn solve_package_specs(
        &self,
        prefix: &Path,
        channel_names: Vec<String>,
        specs: Vec<MatchSpec>,
    ) -> Result<Vec<RepoDataRecord>> {
        let channels = Self::resolve_channels_for_prefix(prefix, channel_names)?;
        let virtual_packages = Self::detect_virtual_packages()?;
        let platforms = [Platform::current(), Platform::NoArch];

        let cache_root = Self::cache_root_dir()?;
        let repo_data_sets: Vec<RepoData> = Gateway::builder()
            .with_cache_dir(cache_root.clone())
            .with_package_cache(PackageCache::new(Self::package_cache_dir(&cache_root)))
            .finish()
            .query(channels, platforms, specs.clone())
            .recursive(true)
            .execute()
            .await
            .map_err(|error| {
                EnvError::Execution(format!("Failed to fetch repodata for solve: {}", error))
            })?;

        if repo_data_sets.iter().all(RepoData::is_empty) {
            return Err(EnvError::Execution(
                "No package metadata was returned for the requested channels and specs".to_string(),
            ));
        }

        let mut solver = RattlerSolver::default();
        let solved = solver
            .solve(SolverTask {
                specs,
                virtual_packages,
                channel_priority: Self::default_channel_priority(),
                ..SolverTask::from_iter(repo_data_sets.iter())
            })
            .map_err(|error| {
                EnvError::Execution(format!("Failed to solve environment: {}", error))
            })?;

        Ok(solved.records)
    }

    async fn install_packages_by_prefix_natively(
        &self,
        prefix: &Path,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }

        if !Self::is_environment_prefix(prefix) {
            return Err(EnvError::Execution(format!(
                "Environment prefix is not a valid conda-style environment: {}",
                prefix.display()
            )));
        }

        let progress = if matches!(output_mode, OutputMode::Summary) {
            Some(Self::summary_spinner(format!(
                "Resolving package install for {}...",
                prefix.display()
            ))?)
        } else {
            None
        };

        if matches!(output_mode, OutputMode::Stream) {
            println!(
                "Resolving package install for {} with rattler...",
                prefix.display()
            );
        }

        let installed = Self::collect_installed_prefix_records(prefix).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to read installed packages from {}: {}",
                prefix.display(),
                error
            ))
        })?;
        let requested_spec_strings = Self::merge_requested_install_specs(
            Self::requested_spec_strings_from_prefix_records(&installed),
            packages,
        );
        let requested_specs = Self::parse_match_specs(&requested_spec_strings)?;
        let solved_records = self
            .solve_package_specs(
                prefix,
                Self::install_channel_hints(&installed, &requested_spec_strings),
                requested_specs.clone(),
            )
            .await?;

        if let Some(pb) = &progress {
            pb.set_message(format!(
                "Installing {} packages into {}...",
                solved_records.len(),
                prefix.display()
            ));
        }
        if matches!(output_mode, OutputMode::Stream) {
            println!(
                "Installing {} packages into {} with rattler...",
                solved_records.len(),
                prefix.display()
            );
        }

        let cache_root = Self::cache_root_dir()?;
        let stashed_ownership_marker = Self::stash_ownership_marker(prefix)?;
        let install_result = Installer::new()
            .with_package_cache(PackageCache::new(Self::package_cache_dir(&cache_root)))
            .with_installed_packages(installed)
            .with_requested_specs(requested_specs)
            .install(prefix, solved_records)
            .await
            .map(|_| ())
            .map_err(|error| {
                EnvError::Execution(format!(
                    "Failed to install solved packages into {}: {}",
                    prefix.display(),
                    error
                ))
            });
        let restore_result = Self::restore_ownership_marker(prefix, stashed_ownership_marker);

        let result = match (install_result, restore_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(install_error), Err(restore_error)) => Err(EnvError::Execution(format!(
                "{}; additionally failed to restore enva ownership marker in {}: {}",
                install_error,
                prefix.display(),
                restore_error
            ))),
        };

        if let Some(pb) = progress {
            match &result {
                Ok(()) => pb.finish_and_clear(),
                Err(error) => pb.abandon_with_message(format!(
                    "✗ Failed package install for {}: {}",
                    prefix.display(),
                    error
                )),
            }
        }

        if result.is_ok() && matches!(output_mode, OutputMode::Summary) {
            println!("✓ Installed packages into {}", prefix.display());
        }

        result
    }

    fn build_prefixed_path(&self, prefix: &Path) -> Result<OsString> {
        let mut path_entries = Vec::new();
        path_entries.push(prefix.join("bin"));

        #[cfg(target_os = "windows")]
        {
            path_entries.push(prefix.join("Scripts"));
            path_entries.push(prefix.join("Library").join("bin"));
        }

        path_entries.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));

        std::env::join_paths(path_entries).map_err(|error| {
            EnvError::Environment(format!(
                "Failed to construct PATH for environment {}: {}",
                prefix.display(),
                error
            ))
        })
    }

    async fn run_command_in_prefix(&self, prefix: &Path, request: &RunRequest) -> Result<()> {
        if !Self::is_environment_prefix(prefix) {
            return Err(EnvError::Execution(format!(
                "Environment prefix is not a valid conda-style environment: {}",
                prefix.display()
            )));
        }

        let env_name = self.environment_name_for_prefix(prefix);
        let mut cmd = AsyncCommand::new("bash");
        cmd.arg("-lc").arg(&request.command);
        cmd.current_dir(&request.cwd);
        cmd.env("PATH", self.build_prefixed_path(prefix)?);
        cmd.env("CONDA_PREFIX", prefix);
        cmd.env("CONDA_DEFAULT_ENV", &env_name);
        cmd.env("CONDA_SHLVL", "1");
        cmd.env("RATTLER_ENV_PREFIX", prefix);

        for env_pair in &request.env_vars {
            let (key, value) = env_pair.split_once('=').ok_or_else(|| {
                EnvError::Validation(format!("Invalid environment variable format: {}", env_pair))
            })?;
            cmd.env(key, value);
        }

        if request.capture_output {
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
        }

        let output = if request.capture_output {
            let output = cmd.output().await.map_err(|error| {
                EnvError::Execution(format!("Failed to execute command: {}", error))
            })?;

            if !output.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }

            output
        } else {
            let status = cmd.status().await.map_err(|error| {
                EnvError::Execution(format!("Failed to execute command: {}", error))
            })?;
            std::process::Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        };

        if !output.status.success() {
            return Err(EnvError::Execution(format!(
                "Command failed with exit code {:?}",
                output.status.code()
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl EnvironmentBackend for RattlerBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Rattler
    }

    async fn clean_package_cache(&self, dry_run: bool, output_mode: OutputMode) -> Result<()> {
        let cache_root = Self::cache_root_dir()?;
        let cache_directories = Self::cache_directory_entries(&cache_root);

        if dry_run {
            if matches!(output_mode, OutputMode::Summary | OutputMode::Stream) {
                println!(
                    "[DRY-RUN] Would clean rattler caches under {}",
                    cache_root.display()
                );
                for (label, path) in &cache_directories {
                    println!("  - {}: {}", label, path.display());
                }
            }
            return Ok(());
        }

        if matches!(output_mode, OutputMode::Summary | OutputMode::Stream) {
            println!("Cleaning rattler caches under {}", cache_root.display());
        }

        let mut removed = Vec::new();
        let mut missing = Vec::new();
        for (label, path) in cache_directories {
            if path.exists() {
                Self::clear_cache_directory(&path).await?;
                removed.push((label, path));
            } else {
                missing.push((label, path));
            }
        }

        if matches!(output_mode, OutputMode::Summary | OutputMode::Stream) {
            for (label, path) in &removed {
                println!("  ✓ Removed {} cache: {}", label, path.display());
            }
            if matches!(output_mode, OutputMode::Stream) {
                for (label, path) in &missing {
                    println!("  - No {} cache present: {}", label, path.display());
                }
            }
            if matches!(output_mode, OutputMode::Summary) {
                println!("✓ Rattler cache cleanup complete");
            }
        }

        Ok(())
    }

    async fn create_environment(
        &self,
        env_name: &str,
        yaml_file: &Path,
        dry_run: bool,
        force: bool,
        output_mode: OutputMode,
    ) -> Result<()> {
        let _lock = self.creation_lock.lock().await;
        let progress = if matches!(output_mode, OutputMode::Summary) {
            Some(Self::summary_spinner(format!(
                "Preparing environment {}...",
                env_name
            ))?)
        } else {
            None
        };

        if dry_run {
            if let Some(pb) = &progress {
                pb.set_message(format!("Validating YAML for {}...", env_name));
            }
            let validation = self.validate_yaml(yaml_file).await?;
            if let Some(pb) = progress {
                pb.finish_and_clear();
            }
            println!("{}", serde_json::to_string_pretty(&validation)?);
            return Ok(());
        }

        if let Some(pb) = &progress {
            pb.set_message(format!("Validating YAML for {}...", env_name));
        }
        let environment_yaml = Self::parse_environment_yaml(yaml_file)?;
        let issues = Self::environment_issues(&environment_yaml);
        if !issues.is_empty() {
            return Err(EnvError::Validation(issues.join("; ")));
        }

        if let Some(pb) = &progress {
            pb.set_message(format!("Resolving target prefix for {}...", env_name));
        }
        let target_prefix = self.target_prefix_for_env_name(env_name)?;
        let conflicting_environments =
            Self::prioritize_named_records(env_name, self.accessible_environment_records().await?)
                .into_iter()
                .filter(|environment| environment.prefix != target_prefix)
                .collect::<Vec<DiscoveredEnvironment>>();

        let native_owned_conflicts = conflicting_environments
            .iter()
            .filter(|environment| Self::has_native_rattler_conflict(environment))
            .collect::<Vec<&DiscoveredEnvironment>>();
        if !native_owned_conflicts.is_empty() {
            return Err(EnvError::Execution(format!(
                "Environment '{}' already exists in other native rattler-owned prefixes: {}. Use --prefix to disambiguate.",
                env_name,
                native_owned_conflicts
                    .iter()
                    .map(|environment| environment.prefix.display().to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            )));
        }

        if !conflicting_environments.is_empty() {
            if !force {
                return Err(EnvError::Execution(format!(
                    "Environment '{}' already exists in other tool-managed or adopted prefixes: {}. Re-run with --force to remove them via their original package manager before recreating with rattler.",
                    env_name,
                    conflicting_environments
                        .iter()
                        .map(|environment| environment.prefix.display().to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                )));
            }

            for environment in &conflicting_environments {
                if let Some(pb) = &progress {
                    pb.set_message(format!(
                        "Removing conflicting {} environment '{}'...",
                        Self::removable_conflict_manager_label(environment)
                            .unwrap_or_else(|| "rattler".to_string()),
                        env_name
                    ));
                }
                if matches!(output_mode, OutputMode::Stream) {
                    println!(
                        "Removing conflicting {} environment '{}' at {} before rattler create...",
                        Self::removable_conflict_manager_label(environment)
                            .unwrap_or_else(|| "rattler".to_string()),
                        env_name,
                        environment.prefix.display()
                    );
                }
                self.remove_conflicting_environment(environment, output_mode)
                    .await?;
            }
        }

        if target_prefix.exists() {
            if Self::is_environment_prefix(&target_prefix) {
                if force {
                    if let Some(pb) = &progress {
                        pb.set_message(format!("Removing existing environment {}...", env_name));
                    }
                    if matches!(output_mode, OutputMode::Stream) {
                        println!(
                            "Removing existing rattler environment '{}' at {}",
                            env_name,
                            target_prefix.display()
                        );
                    }
                    async_fs::remove_dir_all(&target_prefix)
                        .await
                        .map_err(|error| {
                            EnvError::FileOperation(format!(
                                "Failed to remove existing environment {}: {}",
                                target_prefix.display(),
                                error
                            ))
                        })?;
                } else {
                    return Err(EnvError::Execution(format!(
                        "Environment {} already exists. Re-run with --force to replace it.",
                        env_name
                    )));
                }
            } else {
                return Err(EnvError::Execution(format!(
                    "Failed to create environment: Non-conda folder exists at prefix {}",
                    target_prefix.display()
                )));
            }
        }

        if let Some(pb) = &progress {
            pb.set_message(format!("Solving environment {} with rattler...", env_name));
        }
        if matches!(output_mode, OutputMode::Stream) {
            println!("Solving environment {} with rattler...", env_name);
        }
        let (requested_specs, solved_records) =
            self.solve_environment(yaml_file, &environment_yaml).await?;

        if let Some(pb) = &progress {
            pb.set_message(format!(
                "Installing {} solved packages into {}...",
                solved_records.len(),
                target_prefix.display()
            ));
        }
        if matches!(output_mode, OutputMode::Stream) {
            println!(
                "Installing {} solved packages into {}...",
                solved_records.len(),
                target_prefix.display()
            );
        }

        let cache_root = Self::cache_root_dir()?;
        let install_result = Installer::new()
            .with_package_cache(PackageCache::new(Self::package_cache_dir(&cache_root)))
            .with_requested_specs(requested_specs)
            .install(&target_prefix, solved_records)
            .await
            .map(|_| ())
            .map_err(|error| {
                EnvError::Execution(format!(
                    "Failed to install solved packages into {}: {}",
                    target_prefix.display(),
                    error
                ))
            });

        match install_result {
            Ok(()) => {
                write_rattler_ownership_record(&target_prefix, None)?;
                if let Some(pb) = progress {
                    pb.finish_and_clear();
                }
                if matches!(output_mode, OutputMode::Summary) {
                    println!("✓ Environment {} created", env_name);
                }
                Ok(())
            }
            Err(error) => {
                if let Some(pb) = progress {
                    pb.abandon_with_message(format!(
                        "✗ Failed to create environment {}: {}",
                        env_name, error
                    ));
                }
                Err(error)
            }
        }
    }

    async fn validate_yaml(&self, yaml_file: &Path) -> Result<ValidationResult> {
        let environment_yaml = Self::parse_environment_yaml(yaml_file)?;
        let issues = Self::environment_issues(&environment_yaml);
        let syntax_valid = issues.is_empty();
        let estimated_packages = environment_yaml.dependencies.len();

        Ok(ValidationResult {
            dry_run: true,
            environment: environment_yaml
                .name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            yaml_file: yaml_file.to_path_buf(),
            validation: ValidationDetails {
                syntax_valid,
                dependencies_resolvable: issues.is_empty(),
                version_conflicts: issues,
                channels_accessible: !environment_yaml.channels.is_empty(),
            },
            estimated_packages,
            estimated_size_mb: (estimated_packages as u64) * 10,
            channels_accessible: Self::extract_string_list(&environment_yaml),
        })
    }

    async fn environment_exists(&self, env_name: &str) -> Result<bool> {
        Ok(!self.find_environment_prefixes(env_name).await?.is_empty())
    }

    async fn install_packages(
        &self,
        env_name: &str,
        packages: &[String],
        output_mode: OutputMode,
    ) -> Result<()> {
        let environment = self
            .ensure_adopted_environment(&EnvironmentTarget::Name(env_name.to_string()), output_mode)
            .await?;

        if Self::helper_package_manager(&environment).is_none() {
            return self
                .install_packages_by_prefix_natively(&environment.prefix, packages, output_mode)
                .await;
        }

        let manager = self.helper_manager_for_environment(&environment).await?;
        manager
            .install_packages_by_prefix(&environment.prefix, packages, output_mode)
            .await
    }

    async fn adopt_environment(
        &self,
        target: &EnvironmentTarget,
        output_mode: OutputMode,
    ) -> Result<()> {
        self.ensure_adopted_environment(target, output_mode).await?;
        Ok(())
    }

    async fn remove_environment_with_output(
        &self,
        env_name: &str,
        output_mode: OutputMode,
    ) -> Result<()> {
        let environment = self
            .ensure_adopted_environment(&EnvironmentTarget::Name(env_name.to_string()), output_mode)
            .await?;
        self.remove_resolved_environment(environment, env_name, output_mode)
            .await
    }

    async fn remove_environment_by_prefix_with_output(
        &self,
        prefix: &Path,
        output_mode: OutputMode,
    ) -> Result<()> {
        let environment = self
            .ensure_adopted_environment(
                &EnvironmentTarget::Prefix(prefix.to_path_buf()),
                output_mode,
            )
            .await?;
        let display_name = environment.name.clone();
        self.remove_resolved_environment(environment, &display_name, output_mode)
            .await
    }

    async fn get_all_conda_environments(&self) -> Result<Vec<CondaEnvironment>> {
        let mut environments = self
            .accessible_environment_records()
            .await?
            .into_iter()
            .map(|environment| {
                let source = Self::source_label(&environment.source);
                let owner = environment.owner_label().to_string();
                let adopted_from = environment.adopted_from_label();
                CondaEnvironment {
                    name: environment.name,
                    is_active: environment.is_active,
                    prefix: environment.prefix.display().to_string(),
                    source: Some(source),
                    owner: Some(owner),
                    adopted_from,
                }
            })
            .collect::<Vec<CondaEnvironment>>();

        environments.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.prefix.cmp(&right.prefix))
        });
        Ok(environments)
    }

    async fn find_environment_prefixes(&self, env_name: &str) -> Result<Vec<PathBuf>> {
        Ok(
            Self::prioritize_named_records(env_name, self.accessible_environment_records().await?)
                .into_iter()
                .map(|environment| environment.prefix)
                .collect(),
        )
    }

    async fn run(&self, target: &EnvironmentTarget, request: &RunRequest) -> Result<()> {
        let environment = self
            .ensure_adopted_environment(target, OutputMode::Summary)
            .await?;
        self.run_command_in_prefix(&environment.prefix, request)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::RattlerBackend;
    use crate::backend::{EnvironmentBackend, EnvironmentTarget, OutputMode, RunRequest};
    use crate::ownership::write_rattler_ownership_record;
    use crate::package_manager::PackageManager;
    use crate::prefix_registry::{DiscoveredEnvironment, EnvironmentOwner, EnvironmentSource};
    use rattler_conda_types::{PackageName, PackageRecord, PrefixRecord, RepoDataRecord, Version};
    use rattler_solve::ChannelPriority;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn backend_with_root(root: &Path) -> RattlerBackend {
        RattlerBackend::with_root_prefixes(vec![root.to_path_buf()])
    }

    fn create_fake_environment(prefix: &Path) {
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(
            prefix.join("conda-meta").join("history"),
            "created-by-test\n",
        )
        .unwrap();
    }

    fn write_fake_prefix_record(prefix: &Path, name: &str) {
        let package_record = PackageRecord::new(
            PackageName::new_unchecked(name),
            Version::from_str("1.0.0").unwrap(),
            "h123_0".to_string(),
        );
        let repodata_record = RepoDataRecord {
            package_record,
            identifier: format!("{name}-1.0.0-h123_0.conda").parse().unwrap(),
            url: format!(
                "https://conda.anaconda.org/conda-forge/linux-64/{name}-1.0.0-h123_0.conda"
            )
            .parse()
            .unwrap(),
            channel: Some("https://conda.anaconda.org/conda-forge/".to_string()),
        };
        PrefixRecord::from_repodata_record(repodata_record, vec![])
            .write_to_path(
                prefix
                    .join("conda-meta")
                    .join(format!("{name}-1.0.0-h123_0.json")),
                true,
            )
            .unwrap();
    }

    fn discovered_environment(
        name: &str,
        prefix: &str,
        source: EnvironmentSource,
    ) -> DiscoveredEnvironment {
        discovered_environment_with_owner(name, prefix, source, EnvironmentOwner::External, None)
    }

    fn discovered_environment_with_owner(
        name: &str,
        prefix: &str,
        source: EnvironmentSource,
        owner: EnvironmentOwner,
        adopted_from: Option<EnvironmentSource>,
    ) -> DiscoveredEnvironment {
        DiscoveredEnvironment {
            name: name.to_string(),
            prefix: PathBuf::from(prefix),
            is_active: false,
            source,
            owner,
            adopted_from,
        }
    }
    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[tokio::test]
    async fn validate_yaml_accepts_basic_environment_file() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let yaml_file = tempdir.path().join("env.yaml");
        fs::write(
            &yaml_file,
            "name: test-env\nchannels:\n  - conda-forge\ndependencies:\n  - python=3.10\n  - pip\n",
        )
        .unwrap();

        let backend = RattlerBackend::new();
        let result = backend.validate_yaml(&yaml_file).await.unwrap();

        assert_eq!(result.environment, "test-env");
        assert!(result.validation.syntax_valid);
        assert_eq!(result.estimated_packages, 2);
        assert_eq!(result.channels_accessible, vec!["conda-forge".to_string()]);
    }

    #[tokio::test]
    async fn validate_yaml_reports_missing_dependencies_section() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let yaml_file = tempdir.path().join("env.yaml");
        fs::write(&yaml_file, "name: test-env\nchannels:\n  - conda-forge\n").unwrap();

        let backend = RattlerBackend::new();
        let result = backend.validate_yaml(&yaml_file).await.unwrap();

        assert!(!result.validation.syntax_valid);
        assert!(result
            .validation
            .version_conflicts
            .iter()
            .any(|issue| issue.contains("Missing required 'dependencies' section")));
    }

    #[tokio::test]
    async fn validate_yaml_reports_pip_subsection_as_unsupported() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let yaml_file = tempdir.path().join("env.yaml");
        fs::write(
            &yaml_file,
            "name: test-env\nchannels:\n  - conda-forge\ndependencies:\n  - python=3.10\n  - pip:\n    - requests\n",
        )
        .unwrap();

        let backend = RattlerBackend::new();
        let result = backend.validate_yaml(&yaml_file).await.unwrap();

        assert!(!result.validation.syntax_valid);
        assert!(result
            .validation
            .version_conflicts
            .iter()
            .any(|issue| issue.contains("pip subsection")));
    }

    #[test]
    fn dedupe_paths_preserves_detection_order() {
        let ordered = RattlerBackend::dedupe_paths(vec![
            PathBuf::from("/preferred-root"),
            PathBuf::from("/fallback-root"),
            PathBuf::from("/preferred-root"),
        ]);

        assert_eq!(
            ordered,
            vec![
                PathBuf::from("/preferred-root"),
                PathBuf::from("/fallback-root"),
            ]
        );
    }

    #[test]
    fn preferred_root_prefix_uses_first_existing_root() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let preferred = tempdir.path().join("preferred-root");
        let fallback = tempdir.path().join("fallback-root");
        fs::create_dir_all(&preferred).unwrap();
        fs::create_dir_all(&fallback).unwrap();

        let backend = RattlerBackend::with_root_prefixes(vec![
            preferred.clone(),
            fallback.clone(),
            preferred.clone(),
        ]);

        assert_eq!(backend.preferred_root_prefix(), preferred);
    }

    #[test]
    fn target_prefix_uses_envs_subdirectory() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("rattler-root");
        let backend = backend_with_root(&root);

        let target_prefix = backend.target_prefix_for_env_name("test-env").unwrap();
        assert_eq!(target_prefix, root.join("envs").join("test-env"));
    }

    #[tokio::test]
    async fn find_environment_prefixes_returns_named_environment() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("rattler-root");
        let env_prefix = root.join("envs").join("test-env");
        create_fake_environment(&env_prefix);

        let backend = backend_with_root(&root);
        let prefixes = backend.find_environment_prefixes("test-env").await.unwrap();

        assert_eq!(prefixes, vec![env_prefix]);
    }

    #[tokio::test]
    async fn remove_environment_with_output_removes_named_environment() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("rattler-root");
        let env_prefix = root.join("envs").join("test-env");
        create_fake_environment(&env_prefix);

        let backend = backend_with_root(&root);
        backend
            .remove_environment_with_output("test-env", OutputMode::Quiet)
            .await
            .unwrap();

        assert!(!env_prefix.exists());
    }

    #[test]
    fn owned_environment_records_preserve_adopted_source() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("micromamba-root");
        let env_prefix = root.join("envs").join("demo");
        create_fake_environment(&env_prefix);
        write_rattler_ownership_record(&env_prefix, Some("micromamba")).unwrap();

        let backend = backend_with_root(&root);
        let records = backend.owned_environment_records().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].owner, EnvironmentOwner::Rattler);
        assert_eq!(
            records[0].source,
            EnvironmentSource::PackageManager(PackageManager::Micromamba)
        );
        assert_eq!(
            records[0].adopted_from,
            Some(EnvironmentSource::PackageManager(
                PackageManager::Micromamba
            ))
        );
    }

    #[test]
    fn owned_environment_records_leave_unowned_prefixes_external() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("active-root");
        let env_prefix = root.join("envs").join("demo");
        create_fake_environment(&env_prefix);

        let backend = backend_with_root(&root);
        let records = backend.owned_environment_records().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].owner, EnvironmentOwner::External);
        assert_eq!(
            records[0].source,
            EnvironmentSource::PackageManager(PackageManager::None)
        );
        assert_eq!(records[0].adopted_from, None);
    }

    #[test]
    fn helper_package_manager_uses_adopted_source_for_rattler_owned_environment() {
        let environment = discovered_environment_with_owner(
            "demo",
            "/tmp/demo",
            EnvironmentSource::Rattler,
            EnvironmentOwner::Rattler,
            Some(EnvironmentSource::PackageManager(
                PackageManager::Micromamba,
            )),
        );

        assert_eq!(
            RattlerBackend::helper_package_manager(&environment),
            Some(PackageManager::Micromamba)
        );
        assert_eq!(
            RattlerBackend::removable_conflict_manager_label(&environment),
            Some("micromamba".to_string())
        );
        assert!(!RattlerBackend::has_native_rattler_conflict(&environment));
    }

    #[test]
    fn has_native_rattler_conflict_rejects_unadopted_rattler_environment() {
        let environment = discovered_environment_with_owner(
            "demo",
            "/tmp/demo",
            EnvironmentSource::Rattler,
            EnvironmentOwner::Rattler,
            None,
        );

        assert_eq!(RattlerBackend::helper_package_manager(&environment), None);
        assert!(RattlerBackend::has_native_rattler_conflict(&environment));
    }

    #[test]
    fn collect_installed_prefix_records_ignores_ownership_marker() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let prefix = tempdir.path().join("envs").join("demo");
        create_fake_environment(&prefix);
        write_fake_prefix_record(&prefix, "python");
        write_rattler_ownership_record(&prefix, Some("micromamba")).unwrap();

        let records = RattlerBackend::collect_installed_prefix_records(&prefix).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0]
                .repodata_record
                .package_record
                .name
                .as_normalized(),
            "python"
        );
    }

    #[test]
    fn ownership_marker_can_be_stashed_and_restored() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let prefix = tempdir.path().join("envs").join("demo");
        create_fake_environment(&prefix);
        write_fake_prefix_record(&prefix, "python");
        write_rattler_ownership_record(&prefix, Some("micromamba")).unwrap();

        let stashed = RattlerBackend::stash_ownership_marker(&prefix).unwrap();
        assert!(stashed.is_some());
        assert!(!prefix.join("conda-meta").join("enva-rattler.json").exists());

        RattlerBackend::restore_ownership_marker(&prefix, stashed).unwrap();
        assert!(prefix.join("conda-meta").join("enva-rattler.json").exists());
    }

    #[test]
    fn merge_requested_install_specs_deduplicates_and_appends_new_packages() {
        let merged = RattlerBackend::merge_requested_install_specs(
            vec!["python=3.10".to_string(), "samtools>=1.18".to_string()],
            &["samtools>=1.18".to_string(), "bioconda::htseq".to_string()],
        );

        assert_eq!(
            merged,
            vec![
                "python=3.10".to_string(),
                "samtools>=1.18".to_string(),
                "bioconda::htseq".to_string(),
            ]
        );
    }

    #[test]
    fn install_channel_hints_fall_back_to_defaults() {
        let channels = RattlerBackend::install_channel_hints(&[], &[]);

        assert_eq!(
            channels,
            vec!["conda-forge".to_string(), "bioconda".to_string()]
        );
    }

    #[test]
    fn channel_hints_from_spec_strings_keep_explicit_channels() {
        let channels = RattlerBackend::channel_hints_from_spec_strings(&[
            "conda-forge::python=3.10".to_string(),
            "bioconda::htseq".to_string(),
            "samtools>=1.18".to_string(),
        ]);

        assert_eq!(
            channels,
            vec!["conda-forge".to_string(), "bioconda".to_string()]
        );
    }

    #[test]
    fn prioritize_named_records_prefers_rattler_for_same_name() {
        let prioritized = RattlerBackend::prioritize_named_records(
            "demo",
            vec![
                discovered_environment(
                    "demo",
                    "/tmp/external-demo",
                    EnvironmentSource::PackageManager(PackageManager::Micromamba),
                ),
                discovered_environment("demo", "/tmp/rattler-demo", EnvironmentSource::Rattler),
                discovered_environment(
                    "other",
                    "/tmp/other",
                    EnvironmentSource::PackageManager(PackageManager::Conda),
                ),
            ],
        );

        assert_eq!(prioritized.len(), 2);
        assert_eq!(prioritized[0].source, EnvironmentSource::Rattler);
        assert_eq!(prioritized[0].prefix, PathBuf::from("/tmp/rattler-demo"));
        assert_eq!(
            prioritized[1].source,
            EnvironmentSource::PackageManager(PackageManager::Micromamba)
        );
    }

    #[test]
    fn cache_directory_entries_use_expected_subdirectories() {
        let root = PathBuf::from("/tmp/rattler-cache-root");
        let entries = RattlerBackend::cache_directory_entries(&root);
        assert_eq!(
            entries
                .iter()
                .map(|(label, path)| ((*label).to_string(), path.clone()))
                .collect::<Vec<(String, PathBuf)>>(),
            vec![
                ("packages".to_string(), root.join("pkgs")),
                ("repodata".to_string(), root.join("repodata")),
                ("run-exports".to_string(), root.join("run-exports")),
            ]
        );
    }

    #[test]
    fn default_channel_priority_is_disabled() {
        assert_eq!(
            RattlerBackend::default_channel_priority(),
            ChannelPriority::Disabled
        );
    }

    #[test]
    fn cache_root_dir_prefers_explicit_rattler_cache_override() {
        let _guard = env_lock().lock().unwrap();
        let previous_rattler = std::env::var_os("RATTLER_CACHE_DIR");
        let previous_xdg = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("RATTLER_CACHE_DIR", "/tmp/custom-rattler-cache");
        std::env::remove_var("XDG_CACHE_HOME");

        let cache_root = RattlerBackend::cache_root_dir().unwrap();
        assert_eq!(cache_root, PathBuf::from("/tmp/custom-rattler-cache"));

        match previous_rattler {
            Some(value) => std::env::set_var("RATTLER_CACHE_DIR", value),
            None => std::env::remove_var("RATTLER_CACHE_DIR"),
        }
        match previous_xdg {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
    }

    #[test]
    fn cache_root_dir_defaults_to_tmp_when_unset() {
        let _guard = env_lock().lock().unwrap();
        let previous_rattler = std::env::var_os("RATTLER_CACHE_DIR");
        let previous_xdg = std::env::var_os("XDG_CACHE_HOME");
        let previous_tmpdir = std::env::var_os("TMPDIR");
        let previous_user = std::env::var_os("USER");
        let tempdir = tempdir().unwrap();
        std::env::remove_var("RATTLER_CACHE_DIR");
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::set_var("TMPDIR", tempdir.path());
        std::env::set_var("USER", "tester");

        let cache_root = RattlerBackend::cache_root_dir().unwrap();
        assert_eq!(cache_root, tempdir.path().join("enva-rattler-cache-tester"));

        match previous_rattler {
            Some(value) => std::env::set_var("RATTLER_CACHE_DIR", value),
            None => std::env::remove_var("RATTLER_CACHE_DIR"),
        }
        match previous_xdg {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match previous_tmpdir {
            Some(value) => std::env::set_var("TMPDIR", value),
            None => std::env::remove_var("TMPDIR"),
        }
        match previous_user {
            Some(value) => std::env::set_var("USER", value),
            None => std::env::remove_var("USER"),
        }
    }

    #[tokio::test]
    async fn clean_package_cache_removes_rattler_cache_directories() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var_os("RATTLER_CACHE_DIR");
        let tempdir = tempdir().unwrap();
        let cache_root = tempdir.path().join("rattler-cache");
        for (_, path) in RattlerBackend::cache_directory_entries(&cache_root) {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("marker.txt"), "cached\n").unwrap();
        }
        std::env::set_var("RATTLER_CACHE_DIR", &cache_root);

        let backend = RattlerBackend::new();
        backend
            .clean_package_cache(false, OutputMode::Quiet)
            .await
            .unwrap();

        for (_, path) in RattlerBackend::cache_directory_entries(&cache_root) {
            assert!(
                !path.exists(),
                "cache path should be removed: {}",
                path.display()
            );
        }

        match previous {
            Some(value) => std::env::set_var("RATTLER_CACHE_DIR", value),
            None => std::env::remove_var("RATTLER_CACHE_DIR"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_uses_prefix_bin_and_conda_prefix() {
        let _guard = env_lock().lock().unwrap();
        let tempdir = tempdir().unwrap();
        let root = tempdir.path().join("rattler-root");
        let env_prefix = root.join("envs").join("test-env");
        create_fake_environment(&env_prefix);
        fs::create_dir_all(env_prefix.join("bin")).unwrap();

        let tool_path = env_prefix.join("bin").join("rattler-test-tool");
        fs::write(&tool_path, "#!/usr/bin/env bash\nexit 0\n").unwrap();
        make_executable(&tool_path);

        let backend = backend_with_root(&root);
        backend
            .run(
                &EnvironmentTarget::Prefix(PathBuf::from(&env_prefix)),
                &RunRequest {
                    command: format!(
                        "test \"$CONDA_PREFIX\" = '{}' && rattler-test-tool",
                        env_prefix.display()
                    ),
                    env_vars: vec![],
                    cwd: tempdir.path().to_path_buf(),
                    capture_output: true,
                },
            )
            .await
            .unwrap();
    }
}
