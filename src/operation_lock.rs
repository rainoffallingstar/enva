use crate::error::{EnvError, Result};
use chrono::Utc;
use fs4::FileExt;
#[cfg(test)]
use fs4::TryLockError;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockOperation {
    Create,
    Install,
    Remove,
    Adopt,
    Run,
    CacheUse,
    CacheClean,
}

#[derive(Debug, Serialize)]
struct LockMetadata {
    pid: u32,
    operation: LockOperation,
    acquired_at: String,
}

pub struct OperationLock {
    file: File,
}

impl OperationLock {
    pub async fn acquire(lock_path: PathBuf, operation: LockOperation) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::acquire_blocking(lock_path, operation))
            .await
            .map_err(|error| EnvError::Lock(format!("Lock acquisition task failed: {error}")))?
    }

    #[cfg(test)]
    pub fn try_acquire(lock_path: PathBuf, operation: LockOperation) -> Result<Option<Self>> {
        let mut file = Self::open_lock_file(&lock_path)?;
        match FileExt::try_lock(&file) {
            Ok(()) => {
                Self::write_metadata(&mut file, operation)?;
                Ok(Some(Self { file }))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(EnvError::Lock(format!(
                "Failed to acquire operation lock {}: {}",
                lock_path.display(),
                error
            ))),
        }
    }

    fn acquire_blocking(lock_path: PathBuf, operation: LockOperation) -> Result<Self> {
        let mut file = Self::open_lock_file(&lock_path)?;
        FileExt::lock(&file).map_err(|error| {
            EnvError::Lock(format!(
                "Failed to acquire operation lock {}: {}",
                lock_path.display(),
                error
            ))
        })?;
        Self::write_metadata(&mut file, operation)?;
        Ok(Self { file })
    }

    fn open_lock_file(lock_path: &Path) -> Result<File> {
        let parent = lock_path.parent().ok_or_else(|| {
            EnvError::Lock(format!("Lock path has no parent: {}", lock_path.display()))
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            EnvError::Lock(format!(
                "Failed to create lock directory {}: {}",
                parent.display(),
                error
            ))
        })?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|error| {
                EnvError::Lock(format!(
                    "Failed to open operation lock {}: {}",
                    lock_path.display(),
                    error
                ))
            })
    }

    fn write_metadata(file: &mut File, operation: LockOperation) -> Result<()> {
        let metadata = LockMetadata {
            pid: std::process::id(),
            operation,
            acquired_at: Utc::now().to_rfc3339(),
        };
        let serialized = serde_json::to_vec_pretty(&metadata).map_err(|error| {
            EnvError::Lock(format!("Failed to serialize lock metadata: {error}"))
        })?;
        file.set_len(0).map_err(|error| {
            EnvError::Lock(format!("Failed to truncate lock metadata: {error}"))
        })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| EnvError::Lock(format!("Failed to seek lock metadata: {error}")))?;
        file.write_all(&serialized)
            .map_err(|error| EnvError::Lock(format!("Failed to write lock metadata: {error}")))?;
        file.write_all(b"\n").map_err(|error| {
            EnvError::Lock(format!("Failed to terminate lock metadata: {error}"))
        })?;
        file.sync_all()
            .map_err(|error| EnvError::Lock(format!("Failed to sync lock metadata: {error}")))
    }
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::{LockOperation, OperationLock};
    use std::process::Command;
    use tempfile::tempdir;

    #[tokio::test]
    async fn exclusive_lock_blocks_second_holder() {
        let temporary_directory = tempdir().unwrap();
        let lock_path = temporary_directory.path().join("environment.lock");
        let first_lock = OperationLock::acquire(lock_path.clone(), LockOperation::Create)
            .await
            .unwrap();

        let second_lock =
            OperationLock::try_acquire(lock_path.clone(), LockOperation::Remove).unwrap();
        assert!(second_lock.is_none());
        assert!(lock_path.is_file());

        drop(first_lock);
        let third_lock = OperationLock::try_acquire(lock_path, LockOperation::Install).unwrap();
        assert!(third_lock.is_some());
    }

    #[test]
    fn exclusive_lock_blocks_another_process() {
        const CHILD_MODE_VARIABLE: &str = "ENVA_OPERATION_LOCK_CHILD_MODE";
        const LOCK_PATH_VARIABLE: &str = "ENVA_OPERATION_LOCK_CHILD_PATH";

        if let Ok(child_mode) = std::env::var(CHILD_MODE_VARIABLE) {
            let lock_path = std::env::var_os(LOCK_PATH_VARIABLE)
                .map(std::path::PathBuf::from)
                .expect("child lock path must be provided");
            let acquired = OperationLock::try_acquire(lock_path, LockOperation::Install)
                .unwrap()
                .is_some();
            assert_eq!(acquired, child_mode == "available");
            return;
        }

        let temporary_directory = tempdir().unwrap();
        let lock_path = temporary_directory.path().join("cross-process.lock");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let parent_lock = runtime
            .block_on(OperationLock::acquire(
                lock_path.clone(),
                LockOperation::Create,
            ))
            .unwrap();
        let current_test_binary = std::env::current_exe().unwrap();

        let blocked_status = Command::new(&current_test_binary)
            .arg("--exact")
            .arg("operation_lock::tests::exclusive_lock_blocks_another_process")
            .env(CHILD_MODE_VARIABLE, "blocked")
            .env(LOCK_PATH_VARIABLE, &lock_path)
            .status()
            .unwrap();
        assert!(blocked_status.success());

        drop(parent_lock);
        let available_status = Command::new(current_test_binary)
            .arg("--exact")
            .arg("operation_lock::tests::exclusive_lock_blocks_another_process")
            .env(CHILD_MODE_VARIABLE, "available")
            .env(LOCK_PATH_VARIABLE, &lock_path)
            .status()
            .unwrap();
        assert!(available_status.success());
    }
}
