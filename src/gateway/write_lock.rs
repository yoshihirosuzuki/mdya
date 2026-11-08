//! Keeps write operations to one at a time for each data directory, across
//! processes.
//!
//! The lock is an exclusive OS file lock (`File::try_lock`) on a file in the
//! data directory. Dropping the guard releases the lock before closing the
//! file: on Windows, the OS may take a moment to release a lock left on a
//! file that is closed, and the `LockFileEx` documentation recommends
//! releasing locks explicitly.
//!
//! A write operation that stops part way without dropping the guard leaves
//! the lock to the OS, so the next write operation can start without anything
//! being done. On Unix, a process's files are closed when it ends for any
//! reason (POSIX), which releases the lock. On Windows, the OS releases the
//! locks of a process that has ended, but may take a moment, and taking the
//! lock before then may fail.
//!
//! Two spellings of one directory reach the same file, so they share the lock.

use std::fs::{File, TryLockError};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::BoxError;

/// The lock file's name. It starts with the component's name, as the names
/// of everything the components put straight in the data directory do.
const LOCK_FILE: &str = "gateway_write.lock";

#[cfg(test)]
pub(crate) mod test_double;

/// The lock that keeps write operations on one data directory to one at a
/// time. A trait, so that the tests of `operations` can replace it.
#[async_trait]
pub(crate) trait WriteLock: Send + Sync {
    /// Takes the lock without waiting. The lock is held until the returned
    /// guard is dropped.
    async fn try_acquire(&self) -> Result<WriteLockGuard, WriteLockError>;
}

/// The lock, held. Dropping it releases the lock.
pub(crate) struct WriteLockGuard {
    _held: Box<dyn Send + Sync>,
}

impl WriteLockGuard {
    /// Wraps what releases the lock when dropped. A test double wraps
    /// something that only records the release.
    pub(crate) fn new(held: Box<dyn Send + Sync>) -> Self {
        Self { _held: held }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteLockError {
    #[error("another write operation is in progress")]
    InProgress,
    #[error("the write lock cannot be taken")]
    Unavailable { source: BoxError },
}

/// Creates the lock for the data directory.
pub(crate) fn write_lock(data_dir: PathBuf) -> Arc<dyn WriteLock> {
    Arc::new(FileWriteLock { data_dir })
}

struct FileWriteLock {
    data_dir: PathBuf,
}

#[async_trait]
impl WriteLock for FileWriteLock {
    /// Creates the data directory if it does not exist yet, as the write
    /// operation that takes the lock needs it anyway.
    async fn try_acquire(&self) -> Result<WriteLockGuard, WriteLockError> {
        tokio::fs::create_dir_all(&self.data_dir)
            .await
            .map_err(unavailable)?;
        // Opened for writing: whether a lock on a file not open for writing
        // fails is unspecified, and on Windows it fails for append-only.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.data_dir.join(LOCK_FILE))
            .await
            .map_err(unavailable)?
            .into_std()
            .await;
        match file.try_lock() {
            Ok(()) => Ok(WriteLockGuard::new(Box::new(LockedFile(file)))),
            Err(TryLockError::WouldBlock) => Err(WriteLockError::InProgress),
            Err(TryLockError::Error(error)) => Err(unavailable(error)),
        }
    }
}

/// The lock file, locked. Dropping it releases the lock, then closes the file.
struct LockedFile(File);

impl Drop for LockedFile {
    fn drop(&mut self) {
        // Closing the file releases the lock as well, so an error here leaves
        // only the delay that releasing explicitly avoids.
        let _ = self.0.unlock();
    }
}

fn unavailable(error: io::Error) -> WriteLockError {
    WriteLockError::Unavailable {
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    async fn acquire(data_dir: &Path) -> Result<WriteLockGuard, WriteLockError> {
        write_lock(data_dir.to_path_buf()).try_acquire().await
    }

    #[tokio::test]
    async fn the_first_acquire_creates_the_data_directory_and_the_lock_file() {
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("data");

        let guard = acquire(&data_dir).await;

        assert!(guard.is_ok());
        let names: Vec<_> = fs::read_dir(&data_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [LOCK_FILE]);
    }

    #[tokio::test]
    async fn a_second_lock_on_the_same_directory_finds_the_first_in_progress() {
        let data_dir = tempfile::tempdir().unwrap();

        let _first = acquire(data_dir.path()).await.unwrap();
        let second = acquire(data_dir.path()).await;

        assert!(matches!(second, Err(WriteLockError::InProgress)));
    }

    #[tokio::test]
    async fn dropping_the_guard_releases_the_lock() {
        let data_dir = tempfile::tempdir().unwrap();

        let first = acquire(data_dir.path()).await.unwrap();
        drop(first);
        let second = acquire(data_dir.path()).await;

        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn a_lock_file_left_behind_does_not_keep_a_write_operation_from_starting() {
        let data_dir = tempfile::tempdir().unwrap();
        fs::write(data_dir.path().join(LOCK_FILE), b"").unwrap();

        let guard = acquire(data_dir.path()).await;

        assert!(guard.is_ok());
    }

    #[tokio::test]
    async fn another_spelling_of_the_same_directory_shares_the_lock() {
        let data_dir = tempfile::tempdir().unwrap();
        fs::create_dir(data_dir.path().join("sub")).unwrap();
        let other_spelling = data_dir.path().join("sub").join("..");

        let _first = acquire(data_dir.path()).await.unwrap();
        let second = acquire(&other_spelling).await;

        assert!(matches!(second, Err(WriteLockError::InProgress)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symbolic_link_to_the_directory_shares_the_lock() {
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("data");
        fs::create_dir(&data_dir).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&data_dir, &link).unwrap();

        let _first = acquire(&data_dir).await.unwrap();
        let second = acquire(&link).await;

        assert!(matches!(second, Err(WriteLockError::InProgress)));
    }

    #[tokio::test]
    async fn different_directories_do_not_share_the_lock() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();

        let _first = acquire(first_dir.path()).await.unwrap();
        let second = acquire(second_dir.path()).await;

        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn a_data_directory_that_cannot_be_created_makes_the_lock_unavailable() {
        let parent = tempfile::tempdir().unwrap();
        let file_in_the_way = parent.path().join("data");
        fs::write(&file_in_the_way, b"").unwrap();

        let guard = acquire(&file_in_the_way.join("inside")).await;

        assert!(matches!(guard, Err(WriteLockError::Unavailable { .. })));
    }

    #[tokio::test]
    async fn a_lock_file_that_cannot_be_opened_makes_the_lock_unavailable() {
        let data_dir = tempfile::tempdir().unwrap();
        fs::create_dir(data_dir.path().join(LOCK_FILE)).unwrap();

        let guard = acquire(data_dir.path()).await;

        assert!(matches!(guard, Err(WriteLockError::Unavailable { .. })));
    }
}
