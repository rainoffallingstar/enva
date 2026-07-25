use crate::error::{EnvError, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TRANSACTION_JOURNAL_VERSION: u32 = 1;
static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublicationPhase {
    Prepared,
    ExistingMovedToBackup,
    StagingPublished,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TransactionJournal {
    version: u32,
    transaction_id: String,
    final_path: PathBuf,
    staging_path: PathBuf,
    backup_path: Option<PathBuf>,
    phase: PublicationPhase,
}

pub struct StagedPrefix {
    journal_path: PathBuf,
    journal: TransactionJournal,
    commit_started: bool,
    committed: bool,
}

impl StagedPrefix {
    pub fn prepare(final_path: &Path) -> Result<Self> {
        let parent = validated_parent_directory(final_path)?;
        let journal_path = journal_path_for(final_path)?;
        if path_entry_exists(&journal_path)? {
            return Err(EnvError::FileOperation(format!(
                "An unfinished environment publication journal already exists at {}. Recover it while holding the environment operation lock before staging another publication.",
                journal_path.display()
            )));
        }

        let transaction_id = unique_transaction_id();
        let staging_path =
            allocate_unique_sibling_path(final_path, "staging", transaction_id.as_str())?;
        let journal = TransactionJournal {
            version: TRANSACTION_JOURNAL_VERSION,
            transaction_id,
            final_path: final_path.to_path_buf(),
            staging_path: staging_path.clone(),
            backup_path: None,
            phase: PublicationPhase::Prepared,
        };

        create_journal(&journal_path, &journal)?;
        if let Err(error) = fs::create_dir(&staging_path) {
            let _ = remove_journal(&journal_path, parent);
            return Err(EnvError::FileOperation(format!(
                "Failed to create staging directory {}: {}",
                staging_path.display(),
                error
            )));
        }
        sync_directory(parent)?;

        Ok(Self {
            journal_path,
            journal,
            commit_started: false,
            committed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.journal.staging_path
    }

    pub fn recover(final_path: &Path) -> Result<()> {
        let journal_path = journal_path_for(final_path)?;
        if !path_entry_exists(&journal_path)? {
            return Ok(());
        }

        ensure_regular_file(&journal_path, "transaction journal")?;
        let journal = read_latest_journal_record(&journal_path)?;
        validate_journal(final_path, &journal_path, &journal)?;
        recover_journal(&journal_path, &journal)
    }

    pub fn commit(mut self) -> Result<()> {
        let mut publication_hooks = NoopPublicationHooks;
        let final_path = self.journal.final_path.clone();
        let publication_result = self.commit_with_hooks(&mut publication_hooks);
        if let Err(publication_error) = publication_result {
            return match Self::recover(&final_path) {
                Ok(()) => Err(publication_error),
                Err(recovery_error) => Err(EnvError::FileOperation(format!(
                    "{}; additionally failed to recover the interrupted publication for {}: {}",
                    publication_error,
                    final_path.display(),
                    recovery_error
                ))),
            };
        }
        Ok(())
    }

    fn commit_with_hooks(&mut self, publication_hooks: &mut impl PublicationHooks) -> Result<()> {
        self.commit_started = true;
        let parent = validated_parent_directory(&self.journal.final_path)?;

        if directory_entry_exists(&self.journal.final_path, "existing environment")? {
            let backup_path = unique_sibling_path(
                &self.journal.final_path,
                "backup",
                self.journal.transaction_id.as_str(),
            );
            self.journal.backup_path = Some(backup_path.clone());
            append_journal(&self.journal_path, &self.journal)?;

            fs::rename(&self.journal.final_path, &backup_path).map_err(|error| {
                EnvError::FileOperation(format!(
                    "Failed to move existing environment {} to backup {}: {}",
                    self.journal.final_path.display(),
                    backup_path.display(),
                    error
                ))
            })?;
            publication_hooks.after_backup_rename(&self.journal)?;
            publication_hooks.before_parent_sync(PublicationSyncPoint::BackupPublished)?;
            sync_directory(parent)?;
        }

        self.journal.phase = PublicationPhase::ExistingMovedToBackup;
        append_journal(&self.journal_path, &self.journal)?;

        fs::rename(&self.journal.staging_path, &self.journal.final_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to publish staged environment {} to {}: {}",
                self.journal.staging_path.display(),
                self.journal.final_path.display(),
                error
            ))
        })?;
        publication_hooks.after_staging_rename(&self.journal)?;
        publication_hooks.before_parent_sync(PublicationSyncPoint::StagingPublished)?;
        sync_directory(parent)?;

        self.journal.phase = PublicationPhase::StagingPublished;
        append_journal(&self.journal_path, &self.journal)?;

        if let Some(backup_path) = &self.journal.backup_path {
            publication_hooks.before_backup_removal(&self.journal)?;
            remove_directory_entry(backup_path, "previous environment backup")?;
            publication_hooks.before_parent_sync(PublicationSyncPoint::BackupRemoved)?;
            sync_directory(parent)?;
        }

        remove_journal(&self.journal_path, parent)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedPrefix {
    fn drop(&mut self) {
        if self.committed || self.commit_started {
            return;
        }

        let _ = remove_directory_entry(&self.journal.staging_path, "abandoned staging prefix");
        if let Some(parent) = self.journal.final_path.parent() {
            let _ = remove_journal(&self.journal_path, parent);
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PublicationSyncPoint {
    BackupPublished,
    StagingPublished,
    BackupRemoved,
}

trait PublicationHooks {
    fn after_backup_rename(&mut self, _journal: &TransactionJournal) -> Result<()> {
        Ok(())
    }

    fn after_staging_rename(&mut self, _journal: &TransactionJournal) -> Result<()> {
        Ok(())
    }

    fn before_backup_removal(&mut self, _journal: &TransactionJournal) -> Result<()> {
        Ok(())
    }

    fn before_parent_sync(&mut self, _sync_point: PublicationSyncPoint) -> Result<()> {
        Ok(())
    }
}

struct NoopPublicationHooks;

impl PublicationHooks for NoopPublicationHooks {}

fn recover_journal(journal_path: &Path, journal: &TransactionJournal) -> Result<()> {
    let parent = validated_parent_directory(&journal.final_path)?;
    let final_exists = directory_entry_exists(&journal.final_path, "final environment")?;
    let staging_exists = directory_entry_exists(&journal.staging_path, "staging environment")?;
    let backup_exists = journal
        .backup_path
        .as_deref()
        .map(|path| directory_entry_exists(path, "environment backup"))
        .transpose()?
        .unwrap_or(false);

    match journal.phase {
        PublicationPhase::Prepared => {
            if final_exists && backup_exists {
                return inconsistent_journal_state(
                    journal,
                    "both final and backup exist before the backup phase was recorded",
                );
            }

            if backup_exists {
                if final_exists {
                    return inconsistent_journal_state(
                        journal,
                        "cannot restore backup because the final path is occupied",
                    );
                }
                if staging_exists {
                    remove_directory_entry(
                        &journal.staging_path,
                        "interrupted staging environment",
                    )?;
                }
                restore_backup(journal, parent)?;
            } else if !final_exists && journal.backup_path.is_some() {
                return inconsistent_journal_state(
                    journal,
                    "the original environment and its configured backup are both missing",
                );
            } else if staging_exists {
                remove_directory_entry(&journal.staging_path, "interrupted staging environment")?;
            }
        }
        PublicationPhase::ExistingMovedToBackup => match (
            &journal.backup_path,
            final_exists,
            staging_exists,
            backup_exists,
        ) {
            (Some(_), false, _, true) => {
                if staging_exists {
                    remove_directory_entry(
                        &journal.staging_path,
                        "interrupted staging environment",
                    )?;
                }
                restore_backup(journal, parent)?;
            }
            (Some(backup_path), true, false, true) => {
                sync_directory(parent)?;
                remove_directory_entry(backup_path, "superseded environment backup")?;
                sync_directory(parent)?;
            }
            (None, false, true, false) => {
                remove_directory_entry(&journal.staging_path, "interrupted staging environment")?;
            }
            (None, true, false, false) => {}
            _ => {
                return inconsistent_journal_state(
                    journal,
                    "filesystem entries do not identify a safe rollback or completed publication",
                )
            }
        },
        PublicationPhase::StagingPublished => {
            if !final_exists || staging_exists {
                return inconsistent_journal_state(
                    journal,
                    "the published phase requires an existing final environment and no staging directory",
                );
            }
            if let Some(backup_path) = &journal.backup_path {
                if backup_exists {
                    remove_directory_entry(backup_path, "superseded environment backup")?;
                    sync_directory(parent)?;
                }
            }
        }
    }

    remove_journal(journal_path, parent)
}

fn restore_backup(journal: &TransactionJournal, parent: &Path) -> Result<()> {
    let backup_path = journal.backup_path.as_deref().ok_or_else(|| {
        EnvError::FileOperation(format!(
            "Transaction {} has no backup path to restore",
            journal.transaction_id
        ))
    })?;
    fs::rename(backup_path, &journal.final_path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to restore environment backup {} to {}: {}",
            backup_path.display(),
            journal.final_path.display(),
            error
        ))
    })?;
    sync_directory(parent)
}

fn validate_journal(
    expected_final_path: &Path,
    journal_path: &Path,
    journal: &TransactionJournal,
) -> Result<()> {
    if journal.version != TRANSACTION_JOURNAL_VERSION {
        return Err(EnvError::Validation(format!(
            "Unsupported environment transaction journal version {} at {}",
            journal.version,
            journal_path.display()
        )));
    }
    if journal.final_path != expected_final_path {
        return Err(EnvError::PermissionDenied(format!(
            "Transaction journal {} targets {} instead of expected prefix {}",
            journal_path.display(),
            journal.final_path.display(),
            expected_final_path.display()
        )));
    }

    let parent = expected_final_path.parent().ok_or_else(|| {
        EnvError::Validation(format!(
            "Environment prefix has no parent: {}",
            expected_final_path.display()
        ))
    })?;
    validate_transaction_id(journal.transaction_id.as_str(), journal_path)?;
    validate_artifact_path(
        &journal.staging_path,
        parent,
        expected_final_path,
        "staging",
        journal.transaction_id.as_str(),
    )?;
    if let Some(backup_path) = &journal.backup_path {
        validate_artifact_path(
            backup_path,
            parent,
            expected_final_path,
            "backup",
            journal.transaction_id.as_str(),
        )?;
    }
    Ok(())
}

fn validate_transaction_id(transaction_id: &str, journal_path: &Path) -> Result<()> {
    let components = transaction_id.split('-').collect::<Vec<&str>>();
    let has_expected_numeric_components = components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        });
    if !has_expected_numeric_components {
        return Err(EnvError::PermissionDenied(format!(
            "Transaction journal {} contains an invalid transaction ID",
            journal_path.display()
        )));
    }
    Ok(())
}

fn validate_artifact_path(
    artifact_path: &Path,
    expected_parent: &Path,
    final_path: &Path,
    purpose: &str,
    transaction_id: &str,
) -> Result<()> {
    if artifact_path.parent() != Some(expected_parent) {
        return Err(EnvError::PermissionDenied(format!(
            "Transaction {} path escapes environment parent {}: {}",
            purpose,
            expected_parent.display(),
            artifact_path.display()
        )));
    }
    let environment_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            EnvError::Validation(format!(
                "Environment prefix has no UTF-8 name: {}",
                final_path.display()
            ))
        })?;
    let expected_prefix = format!(".enva-{}-{}-{}-", purpose, environment_name, transaction_id);
    let artifact_name = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            EnvError::PermissionDenied(format!(
                "Transaction {} path has no UTF-8 file name: {}",
                purpose,
                artifact_path.display()
            ))
        })?;
    let attempt_suffix = artifact_name
        .strip_prefix(&expected_prefix)
        .ok_or_else(|| {
            EnvError::PermissionDenied(format!(
                "Transaction {} path does not match transaction {}: {}",
                purpose,
                transaction_id,
                artifact_path.display()
            ))
        })?;
    if attempt_suffix.is_empty() || !attempt_suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EnvError::PermissionDenied(format!(
            "Transaction {} path has an invalid allocation suffix: {}",
            purpose,
            artifact_path.display()
        )));
    }
    Ok(())
}

fn create_journal(journal_path: &Path, journal: &TransactionJournal) -> Result<()> {
    let mut journal_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(journal_path)
        .map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to create transaction journal {}: {}",
                journal_path.display(),
                error
            ))
        })?;
    write_journal_record(&mut journal_file, journal, journal_path)
}

fn append_journal(journal_path: &Path, journal: &TransactionJournal) -> Result<()> {
    ensure_regular_file(journal_path, "transaction journal")?;
    let mut journal_file = OpenOptions::new()
        .append(true)
        .open(journal_path)
        .map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to open transaction journal {} for append: {}",
                journal_path.display(),
                error
            ))
        })?;
    write_journal_record(&mut journal_file, journal, journal_path)
}

fn write_journal_record(
    journal_file: &mut File,
    journal: &TransactionJournal,
    journal_path: &Path,
) -> Result<()> {
    let serialized = serde_json::to_vec(journal).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to serialize transaction journal {}: {}",
            journal_path.display(),
            error
        ))
    })?;
    journal_file.write_all(&serialized).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to write transaction journal {}: {}",
            journal_path.display(),
            error
        ))
    })?;
    journal_file.write_all(b"\n").map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to terminate transaction journal record {}: {}",
            journal_path.display(),
            error
        ))
    })?;
    journal_file.sync_all().map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to sync transaction journal {}: {}",
            journal_path.display(),
            error
        ))
    })
}

fn read_latest_journal_record(journal_path: &Path) -> Result<TransactionJournal> {
    let contents = fs::read(journal_path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to read transaction journal {}: {}",
            journal_path.display(),
            error
        ))
    })?;
    let ends_with_newline = contents.last() == Some(&b'\n');
    let records = contents
        .split(|byte| *byte == b'\n')
        .collect::<Vec<&[u8]>>();
    let complete_record_count = records.len().saturating_sub(1);
    let mut latest_record = None;

    for (index, record) in records.iter().enumerate() {
        if record.is_empty() {
            continue;
        }
        match serde_json::from_slice::<TransactionJournal>(record) {
            Ok(journal) => latest_record = Some(journal),
            Err(error) => {
                let is_incomplete_tail = !ends_with_newline && index >= complete_record_count;
                if !is_incomplete_tail || latest_record.is_none() {
                    return Err(EnvError::Validation(format!(
                        "Transaction journal {} contains an invalid record: {}",
                        journal_path.display(),
                        error
                    )));
                }
            }
        }
    }

    latest_record.ok_or_else(|| {
        EnvError::Validation(format!(
            "Transaction journal {} contains no valid records",
            journal_path.display()
        ))
    })
}

fn journal_path_for(final_path: &Path) -> Result<PathBuf> {
    let parent = final_path.parent().ok_or_else(|| {
        EnvError::Validation(format!(
            "Environment prefix has no parent: {}",
            final_path.display()
        ))
    })?;
    let environment_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            EnvError::Validation(format!(
                "Environment prefix has no UTF-8 name: {}",
                final_path.display()
            ))
        })?;
    Ok(parent.join(format!(".enva-transaction-{}.jsonl", environment_name)))
}

fn validated_parent_directory(final_path: &Path) -> Result<&Path> {
    let parent = final_path.parent().ok_or_else(|| {
        EnvError::FileOperation(format!(
            "Environment prefix has no parent: {}",
            final_path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to create environment parent {}: {}",
            parent.display(),
            error
        ))
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to inspect environment parent {}: {}",
            parent.display(),
            error
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EnvError::PermissionDenied(format!(
            "Refusing to publish an environment under non-directory or symlinked parent {}",
            parent.display()
        )));
    }
    Ok(parent)
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(EnvError::FileOperation(format!(
            "Failed to inspect path {}: {}",
            path.display(),
            error
        ))),
    }
}

fn directory_entry_exists(path: &Path, description: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(EnvError::PermissionDenied(format!(
                "Refusing to use {} because it is not a regular directory: {}",
                description,
                path.display()
            )))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(EnvError::FileOperation(format!(
            "Failed to inspect {} {}: {}",
            description,
            path.display(),
            error
        ))),
    }
}

fn ensure_regular_file(path: &Path, description: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to inspect {} {}: {}",
            description,
            path.display(),
            error
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(EnvError::PermissionDenied(format!(
            "Refusing to use {} because it is not a regular file: {}",
            description,
            path.display()
        )));
    }
    Ok(())
}

fn remove_directory_entry(path: &Path, description: &str) -> Result<()> {
    if !directory_entry_exists(path, description)? {
        return Ok(());
    }
    fs::remove_dir_all(path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to remove {} {}: {}",
            description,
            path.display(),
            error
        ))
    })
}

fn remove_journal(journal_path: &Path, parent: &Path) -> Result<()> {
    if path_entry_exists(journal_path)? {
        ensure_regular_file(journal_path, "transaction journal")?;
        fs::remove_file(journal_path).map_err(|error| {
            EnvError::FileOperation(format!(
                "Failed to remove transaction journal {}: {}",
                journal_path.display(),
                error
            ))
        })?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn allocate_unique_sibling_path(
    final_path: &Path,
    purpose: &str,
    transaction_id: &str,
) -> Result<PathBuf> {
    for attempt in 0..128_u32 {
        let candidate =
            unique_sibling_path_with_attempt(final_path, purpose, transaction_id, attempt);
        if !path_entry_exists(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(EnvError::FileOperation(format!(
        "Failed to allocate a unique staging path next to {}",
        final_path.display()
    )))
}

fn unique_sibling_path(final_path: &Path, purpose: &str, transaction_id: &str) -> PathBuf {
    unique_sibling_path_with_attempt(final_path, purpose, transaction_id, 0)
}

fn unique_sibling_path_with_attempt(
    final_path: &Path,
    purpose: &str,
    transaction_id: &str,
    attempt: u32,
) -> PathBuf {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let name = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("environment");
    parent.join(format!(
        ".enva-{}-{}-{}-{}",
        purpose, name, transaction_id, attempt
    ))
}

fn unique_transaction_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{}", std::process::id(), timestamp, sequence)
}

fn inconsistent_journal_state<T>(journal: &TransactionJournal, detail: &str) -> Result<T> {
    Err(EnvError::Validation(format!(
        "Transaction {} for {} is inconsistent: {}. Refusing destructive recovery.",
        journal.transaction_id,
        journal.final_path.display(),
        detail
    )))
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory = File::open(path).map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to open directory {} for sync: {}",
            path.display(),
            error
        ))
    })?;
    directory.sync_all().map_err(|error| {
        EnvError::FileOperation(format!(
            "Failed to sync directory {}: {}",
            path.display(),
            error
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        append_journal, journal_path_for, read_latest_journal_record, PublicationHooks,
        PublicationPhase, PublicationSyncPoint, StagedPrefix, TransactionJournal,
    };
    use crate::error::{EnvError, Result};
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn commit_replaces_existing_environment_and_removes_recovery_artifacts() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("old.txt"), "old\n").unwrap();

        let staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        fs::write(staged_prefix.path().join("new.txt"), "new\n").unwrap();
        staged_prefix.commit().unwrap();

        assert!(!final_path.join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(final_path.join("new.txt")).unwrap(),
            "new\n"
        );
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn dropping_uncommitted_prefix_removes_staging_and_journal() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        let staging_path = {
            let staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
            let path = staged_prefix.path().to_path_buf();
            fs::write(path.join("partial.txt"), "partial\n").unwrap();
            path
        };

        assert!(!staging_path.exists());
        assert!(!final_path.exists());
        assert!(!journal_path_for(&final_path).unwrap().exists());
    }

    #[test]
    fn failed_publication_restores_existing_environment() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("old.txt"), "old\n").unwrap();

        let staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        let staging_path = staged_prefix.path().to_path_buf();
        fs::write(staging_path.join("new.txt"), "new\n").unwrap();
        fs::remove_dir_all(&staging_path).unwrap();

        let result = staged_prefix.commit();

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(final_path.join("old.txt")).unwrap(),
            "old\n"
        );
        assert!(!final_path.join("new.txt").exists());
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn process_interruption_after_backup_rename_is_recovered_on_next_operation() {
        const CHILD_MODE_VARIABLE: &str = "ENVA_STAGED_PREFIX_CRASH_CHILD";
        const FINAL_PATH_VARIABLE: &str = "ENVA_STAGED_PREFIX_CRASH_FINAL_PATH";

        if std::env::var_os(CHILD_MODE_VARIABLE).is_some() {
            let final_path = std::env::var_os(FINAL_PATH_VARIABLE)
                .map(std::path::PathBuf::from)
                .expect("child final path must be provided");
            let mut staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
            fs::write(staged_prefix.path().join("new.txt"), "new\n").unwrap();
            let mut hooks = ExitAfterBackupRename;
            let _ = staged_prefix.commit_with_hooks(&mut hooks);
            panic!("child should have exited after the backup rename");
        }

        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("old.txt"), "old\n").unwrap();
        let current_test_binary = std::env::current_exe().unwrap();

        let child_status = Command::new(current_test_binary)
            .arg("--exact")
            .arg("staged_prefix::tests::process_interruption_after_backup_rename_is_recovered_on_next_operation")
            .env(CHILD_MODE_VARIABLE, "1")
            .env(FINAL_PATH_VARIABLE, &final_path)
            .status()
            .unwrap();
        assert_eq!(child_status.code(), Some(86));
        assert!(!final_path.exists());
        assert!(journal_path_for(&final_path).unwrap().exists());

        StagedPrefix::recover(&final_path).unwrap();
        StagedPrefix::recover(&final_path).unwrap();

        assert_eq!(
            fs::read_to_string(final_path.join("old.txt")).unwrap(),
            "old\n"
        );
        assert!(!final_path.join("new.txt").exists());
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn parent_sync_failure_restores_old_environment() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("old.txt"), "old\n").unwrap();
        let mut staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        fs::write(staged_prefix.path().join("new.txt"), "new\n").unwrap();
        let mut hooks = FailAtSyncPoint {
            target: PublicationSyncPoint::BackupPublished,
        };

        let result = staged_prefix.commit_with_hooks(&mut hooks);
        assert!(result.is_err());
        StagedPrefix::recover(&final_path).unwrap();

        assert_eq!(
            fs::read_to_string(final_path.join("old.txt")).unwrap(),
            "old\n"
        );
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn backup_cleanup_failure_is_recovered_without_losing_published_environment() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("old.txt"), "old\n").unwrap();
        let mut staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        fs::write(staged_prefix.path().join("new.txt"), "new\n").unwrap();
        let mut hooks = FailBeforeBackupRemoval;

        let result = staged_prefix.commit_with_hooks(&mut hooks);
        assert!(result.is_err());
        StagedPrefix::recover(&final_path).unwrap();

        assert_eq!(
            fs::read_to_string(final_path.join("new.txt")).unwrap(),
            "new\n"
        );
        assert!(!final_path.join("old.txt").exists());
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn recovery_accepts_a_truncated_final_journal_record() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        let staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        let journal_path = journal_path_for(&final_path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .unwrap()
            .write_all(br#"{\"version\":1,\"transaction_id\""#)
            .unwrap();
        std::mem::forget(staged_prefix);

        let journal = read_latest_journal_record(&journal_path).unwrap();
        assert_eq!(journal.phase, PublicationPhase::Prepared);
        StagedPrefix::recover(&final_path).unwrap();
        assert_no_transaction_artifacts(&final_path);
    }

    #[test]
    fn recovery_rejects_a_journal_that_targets_another_parent() {
        let temporary_directory = tempdir().unwrap();
        let final_path = temporary_directory.path().join("envs").join("demo");
        let staged_prefix = StagedPrefix::prepare(&final_path).unwrap();
        let journal_path = journal_path_for(&final_path).unwrap();
        let mut journal: TransactionJournal = read_latest_journal_record(&journal_path).unwrap();
        journal.staging_path = temporary_directory.path().join("outside").join("staging");
        append_journal(&journal_path, &journal).unwrap();
        std::mem::forget(staged_prefix);

        let result = StagedPrefix::recover(&final_path);
        assert!(result.is_err());
        assert!(final_path.parent().unwrap().exists());
        assert!(journal_path.exists());
    }

    fn assert_no_transaction_artifacts(final_path: &std::path::Path) {
        let sibling_names = fs::read_dir(final_path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<String>>();
        assert!(sibling_names.iter().all(|name| !name.contains("backup")));
        assert!(sibling_names.iter().all(|name| !name.contains("staging")));
        assert!(sibling_names
            .iter()
            .all(|name| !name.contains("transaction")));
    }

    struct ExitAfterBackupRename;

    impl PublicationHooks for ExitAfterBackupRename {
        fn after_backup_rename(&mut self, _journal: &TransactionJournal) -> Result<()> {
            std::process::exit(86);
        }
    }

    struct FailAtSyncPoint {
        target: PublicationSyncPoint,
    }

    impl PublicationHooks for FailAtSyncPoint {
        fn before_parent_sync(&mut self, sync_point: PublicationSyncPoint) -> Result<()> {
            if sync_point == self.target {
                return Err(EnvError::FileOperation(format!(
                    "injected parent sync failure at {sync_point:?}"
                )));
            }
            Ok(())
        }
    }

    struct FailBeforeBackupRemoval;

    impl PublicationHooks for FailBeforeBackupRemoval {
        fn before_backup_removal(&mut self, _journal: &TransactionJournal) -> Result<()> {
            Err(EnvError::FileOperation(
                "injected backup cleanup failure".to_string(),
            ))
        }
    }

    use std::io::Write;
}
