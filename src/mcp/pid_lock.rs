//! Exclusive pid-file lock for the MCP HTTP daemon.
//!
//! `mdya mcp --http` runs in the foreground; backgrounding is delegated to the
//! shell (`&` / nohup / systemd). To make a backgrounded daemon discoverable
//! across shell sessions, and to forbid a second daemon for the same
//! `~/.mdya/`, the daemon holds an exclusive advisory lock on
//! `<config_dir>/mcp.pid` for its whole lifetime and writes its pid there.
//!
//! The *lock* — not the file's presence — enforces single-instance. The OS
//! releases an advisory lock when the holder exits *or* crashes, so a stale
//! pid file left behind by `kill -9` never blocks the next start: the new
//! daemon re-locks the file and overwrites the stale pid. A second daemon
//! against the same config dir fails fast with [`PidLockError::AlreadyRunning`].

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

use fd_lock::{RwLock, RwLockWriteGuard};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PidLockError {
    #[error("another mdya MCP daemon is already running for this config dir (lock held on {0})")]
    AlreadyRunning(PathBuf),
    #[error("open pid file {path}: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("write pid file {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// Owns the pid-file handle and deletes the file on drop. The exclusive lock
/// is held through a [`RwLockWriteGuard`] borrowed from this value, so the
/// caller must keep both the `PidFile` and the guard alive for the daemon's
/// lifetime; see [`PidFile::lock_exclusive`]. fd-lock's guard borrows its
/// `RwLock`, which is why the two cannot collapse into one owned handle.
pub struct PidFile {
    lock: RwLock<File>,
    path: PathBuf,
}

impl PidFile {
    /// Open the pid file (creating it if absent) without locking or truncating
    /// it — a pre-existing stale pid stays readable until [`write_current_pid`]
    /// replaces it post-bind.
    pub fn open(path: PathBuf) -> Result<Self, PidLockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| PidLockError::Open {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            lock: RwLock::new(file),
            path,
        })
    }

    /// Take the exclusive advisory lock without blocking. The returned guard
    /// must be held for the process lifetime; dropping it releases the lock.
    /// A lock already held by another process maps to
    /// [`PidLockError::AlreadyRunning`].
    pub fn lock_exclusive(&mut self) -> Result<RwLockWriteGuard<'_, File>, PidLockError> {
        match self.lock.try_write() {
            Ok(guard) => Ok(guard),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                Err(PidLockError::AlreadyRunning(self.path.clone()))
            }
            Err(source) => Err(PidLockError::Open {
                path: self.path.clone(),
                source,
            }),
        }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // The handle in `self.lock` is still open here — fields drop *after*
        // this body — but removal is fine regardless: on Unix the inode just
        // unlinks while the fd stays valid, and Rust opens files with
        // FILE_SHARE_DELETE on Windows so an open handle does not block it. The
        // advisory lock is released a moment later when that handle closes;
        // this removal is cosmetic, so an error (e.g. already gone) is ignored.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Overwrite the locked pid file with the current process id. Call *after* the
/// listener has bound so the file only ever names a daemon that is actually
/// accepting connections. `file` is the locked handle (a [`RwLockWriteGuard`]
/// deref-coerces here).
pub fn write_current_pid(file: &mut File, path: &Path) -> Result<(), PidLockError> {
    let wrap = |source| PidLockError::Write {
        path: path.to_path_buf(),
        source,
    };
    file.set_len(0).map_err(wrap)?;
    file.rewind().map_err(wrap)?;
    write!(file, "{}", std::process::id()).map_err(wrap)?;
    file.flush().map_err(wrap)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn lock_then_write_records_current_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.pid");

        let mut pid_file = PidFile::open(path.clone()).expect("open");
        let mut guard = pid_file.lock_exclusive().expect("lock");
        write_current_pid(&mut guard, &path).expect("write pid");

        let mut contents = String::new();
        File::open(&path)
            .expect("reopen")
            .read_to_string(&mut contents)
            .expect("read");
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn second_lock_on_same_path_reports_already_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.pid");

        let mut first = PidFile::open(path.clone()).expect("open first");
        let _held = first.lock_exclusive().expect("first lock");

        // A distinct open file description on the same path: flock treats the
        // two independently even within one process, so the second contends.
        let mut second = PidFile::open(path.clone()).expect("open second");
        let err = second.lock_exclusive().expect_err("second must fail");
        assert!(matches!(err, PidLockError::AlreadyRunning(p) if p == path));
    }

    #[test]
    fn drop_removes_the_pid_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.pid");

        {
            let mut pid_file = PidFile::open(path.clone()).expect("open");
            let mut guard = pid_file.lock_exclusive().expect("lock");
            write_current_pid(&mut guard, &path).expect("write");
            assert!(path.exists(), "pid file should exist while held");
        }
        assert!(!path.exists(), "pid file should be removed on drop");
    }
}
