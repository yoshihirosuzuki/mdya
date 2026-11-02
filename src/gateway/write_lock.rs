//! Keeps write operations to one at a time for each data directory, across
//! processes.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::BoxError;

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
    let _ = data_dir;
    todo!()
}
