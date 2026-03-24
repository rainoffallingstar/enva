use crate::error::{EnvError, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const OWNERSHIP_FILE_NAME: &str = "enva-rattler.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnershipRecord {
    pub version: u8,
    pub owner: String,
    pub adopted_from: Option<String>,
    pub adopted_at: String,
}

impl OwnershipRecord {
    pub fn is_rattler_owned(&self) -> bool {
        self.owner.eq_ignore_ascii_case("rattler")
    }
}

pub fn ownership_record_path(prefix: &Path) -> PathBuf {
    prefix.join("conda-meta").join(OWNERSHIP_FILE_NAME)
}

pub fn read_ownership_record(prefix: &Path) -> Result<Option<OwnershipRecord>> {
    let record_path = ownership_record_path(prefix);
    if !record_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&record_path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to read ownership marker {}: {}",
            record_path.display(),
            error
        ))
    })?;

    let record = serde_json::from_str(&content).map_err(|error| {
        EnvError::Validation(format!(
            "Failed to parse ownership marker {}: {}",
            record_path.display(),
            error
        ))
    })?;

    Ok(Some(record))
}

pub fn write_rattler_ownership_record(
    prefix: &Path,
    adopted_from: Option<&str>,
) -> Result<OwnershipRecord> {
    let conda_meta = prefix.join("conda-meta");
    if !conda_meta.is_dir() {
        return Err(EnvError::Execution(format!(
            "Cannot mark {} as rattler-managed because conda-meta/ is missing",
            prefix.display()
        )));
    }

    let record = OwnershipRecord {
        version: 1,
        owner: "rattler".to_string(),
        adopted_from: adopted_from.map(str::to_string),
        adopted_at: Utc::now().to_rfc3339(),
    };

    let record_path = ownership_record_path(prefix);
    let serialized = serde_json::to_string_pretty(&record).map_err(|error| {
        EnvError::Validation(format!(
            "Failed to serialize ownership marker for {}: {}",
            prefix.display(),
            error
        ))
    })?;

    fs::write(&record_path, serialized).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to write ownership marker {}: {}",
            record_path.display(),
            error
        ))
    })?;

    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{ownership_record_path, read_ownership_record, write_rattler_ownership_record};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn write_and_read_rattler_ownership_record() {
        let tempdir = tempdir().unwrap();
        let prefix = tempdir.path().join("envs").join("demo");
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();

        let written = write_rattler_ownership_record(&prefix, Some("micromamba")).unwrap();
        let read_back = read_ownership_record(&prefix).unwrap().unwrap();

        assert_eq!(written.owner, "rattler");
        assert_eq!(read_back.adopted_from.as_deref(), Some("micromamba"));
        assert!(ownership_record_path(&prefix).exists());
    }
}
