use super::cli::CliBackend;
use super::rattler::RattlerBackend;
use super::{BackendSelector, EnvironmentBackend};
use crate::error::Result;
use std::sync::Arc;

pub async fn build_backend(selector: BackendSelector) -> Result<Arc<dyn EnvironmentBackend>> {
    match selector.kind {
        super::BackendKind::Cli => Ok(Arc::new(CliBackend::new(selector.package_manager))),
        super::BackendKind::Rattler => Ok(Arc::new(RattlerBackend::new())),
    }
}

pub async fn build_default_backend() -> Result<Arc<dyn EnvironmentBackend>> {
    build_backend(BackendSelector::from_env()).await
}
