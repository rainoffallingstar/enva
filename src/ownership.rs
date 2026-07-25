use crate::error::{EnvError, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const OWNERSHIP_FILE_NAME: &str = "enva-rattler.json";
static OWNERSHIP_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let mut serialized = serde_json::to_vec_pretty(&record).map_err(|error| {
        EnvError::Validation(format!(
            "Failed to serialize ownership marker for {}: {}",
            prefix.display(),
            error
        ))
    })?;
    serialized.push(b'\n');
    write_file_atomically(&record_path, &serialized, "ownership marker")?;

    Ok(record)
}

fn write_file_atomically(path: &Path, content: &[u8], label: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        EnvError::FileOperation(format!("{} path has no parent: {}", label, path.display()))
    })?;
    let temporary_path = unique_sibling_path(path, "part");
    let mut temporary_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to create temporary {} {}: {}",
                label,
                temporary_path.display(),
                error
            ))
        })?;

    let write_result = (|| -> std::io::Result<()> {
        temporary_file.write_all(content)?;
        temporary_file.sync_all()?;
        temporary_file.sync_data()?;
        Ok(())
    })();
    drop(temporary_file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(EnvError::FileOperation(format!(
            "Failed to write temporary {} {}: {}",
            label,
            temporary_path.display(),
            error
        )));
    }

    if let Err(error) = replace_file(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(EnvError::FileOperation(format!(
            "Failed to publish {} {}: {}",
            label,
            path.display(),
            error
        )));
    }
    sync_parent_directory(parent, label)
}

#[cfg(not(windows))]
fn replace_file(temporary_path: &Path, final_path: &Path) -> std::io::Result<()> {
    fs::rename(temporary_path, final_path)
}

#[cfg(windows)]
fn replace_file(temporary_path: &Path, final_path: &Path) -> std::io::Result<()> {
    if !final_path.exists() {
        return fs::rename(temporary_path, final_path);
    }

    let backup_path = unique_sibling_path(final_path, "backup");
    fs::rename(final_path, &backup_path)?;
    if let Err(error) = fs::rename(temporary_path, final_path) {
        let _ = fs::rename(&backup_path, final_path);
        return Err(error);
    }
    fs::remove_file(backup_path)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path, label: &str) -> Result<()> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to sync {} parent directory {}: {}",
                label,
                parent.display(),
                error
            ))
        })
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path, _label: &str) -> Result<()> {
    Ok(())
}

fn unique_sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("marker");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = OWNERSHIP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{}-{}-{}-{}-{}",
        file_name,
        suffix,
        std::process::id(),
        timestamp,
        sequence
    ))
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

    #[test]
    fn repeated_write_atomically_replaces_existing_record() {
        let tempdir = tempdir().unwrap();
        let prefix = tempdir.path().join("envs").join("demo");
        let conda_meta = prefix.join("conda-meta");
        fs::create_dir_all(&conda_meta).unwrap();

        write_rattler_ownership_record(&prefix, Some("conda")).unwrap();
        write_rattler_ownership_record(&prefix, Some("micromamba")).unwrap();
        let record = read_ownership_record(&prefix).unwrap().unwrap();

        assert_eq!(record.adopted_from.as_deref(), Some("micromamba"));
        let temporary_entries = fs::read_dir(conda_meta)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("-part-") || name.contains("-backup-"))
            .collect::<Vec<String>>();
        assert!(temporary_entries.is_empty());
    }

    #[test]
    fn corrupted_record_returns_validation_error() {
        let tempdir = tempdir().unwrap();
        let prefix = tempdir.path().join("envs").join("demo");
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(ownership_record_path(&prefix), "{broken-json\n").unwrap();

        let error = read_ownership_record(&prefix).unwrap_err();
        assert!(error
            .to_string()
            .contains("Failed to parse ownership marker"));
    }
}
