//! A test double for [`WriteLock`], for the tests of `operations`.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{WriteLock, WriteLockError, WriteLockGuard};

type AcquireFn = dyn Fn() -> Result<(), WriteLockError> + Send + Sync;

/// Behaves like the lock within one process: `try_acquire` fails with
/// `InProgress` while a guard it handed out is alive. A handler set with
/// [`TestWriteLock::on_try_acquire`] is asked first, so that a test can make
/// it fail as if another process held the lock. Records how often the lock
/// was taken and whether it is held, so that a test can check that an
/// operation holds it while it writes and releases it when it ends.
pub(crate) struct TestWriteLock {
    acquire: Box<AcquireFn>,
    state: Arc<Mutex<LockState>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LockState {
    /// How many times the lock was taken.
    pub(crate) acquired: u32,
    /// Whether a guard is alive now.
    pub(crate) held: bool,
}

impl TestWriteLock {
    pub(crate) fn new() -> Self {
        Self {
            acquire: Box::new(|| Ok(())),
            state: Arc::default(),
        }
    }

    /// Asks `f` before every `try_acquire`; its error is returned and no
    /// guard is handed out.
    pub(crate) fn on_try_acquire(
        mut self,
        f: impl Fn() -> Result<(), WriteLockError> + Send + Sync + 'static,
    ) -> Self {
        self.acquire = Box::new(f);
        self
    }

    pub(crate) fn state(&self) -> LockState {
        *self.state.lock().unwrap()
    }
}

impl Default for TestWriteLock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WriteLock for TestWriteLock {
    async fn try_acquire(&self) -> Result<WriteLockGuard, WriteLockError> {
        (self.acquire)()?;
        let mut state = self.state.lock().unwrap();
        if state.held {
            return Err(WriteLockError::InProgress);
        }
        state.acquired += 1;
        state.held = true;
        Ok(WriteLockGuard::new(Box::new(Release(Arc::clone(
            &self.state,
        )))))
    }
}

/// Marks the lock as released when the guard is dropped. At most one guard
/// is alive at a time.
struct Release(Arc<Mutex<LockState>>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.lock().unwrap().held = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_lock_is_held_until_the_guard_is_dropped() {
        let lock = TestWriteLock::new();

        let guard = lock.try_acquire().await.unwrap();
        let while_held = lock.state();
        drop(guard);

        assert_eq!(
            while_held,
            LockState {
                acquired: 1,
                held: true
            }
        );
        assert_eq!(
            lock.state(),
            LockState {
                acquired: 1,
                held: false
            }
        );
    }

    #[tokio::test]
    async fn a_second_acquire_while_held_is_in_progress() {
        let lock = TestWriteLock::new();

        let _guard = lock.try_acquire().await.unwrap();
        let second = lock.try_acquire().await;

        assert!(matches!(second, Err(WriteLockError::InProgress)));
        assert_eq!(
            lock.state(),
            LockState {
                acquired: 1,
                held: true
            }
        );
    }

    #[tokio::test]
    async fn the_lock_can_be_taken_again_after_release() {
        let lock = TestWriteLock::new();

        drop(lock.try_acquire().await.unwrap());
        let again = lock.try_acquire().await;

        assert!(again.is_ok());
        assert_eq!(lock.state().acquired, 2);
    }

    #[tokio::test]
    async fn a_failing_handler_hands_out_no_guard() {
        let lock = TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress));

        let result = lock.try_acquire().await;

        assert!(matches!(result, Err(WriteLockError::InProgress)));
        assert_eq!(lock.state(), LockState::default());
    }
}
