//! The file in the data directory that holds the settings.

use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "config.json";

pub(super) struct SettingsFile {
    directory: PathBuf,
}

impl SettingsFile {
    pub(super) fn in_directory(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The file's content, or `None` when there is no file yet.
    pub(super) async fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match tokio::fs::read(self.directory.join(FILE_NAME)).await {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replaces the file's content, making the directory if it is not there.
    /// A reader, and whatever is left if the process stops, finds the file as
    /// it was or the whole new content, never part of it.
    pub(super) async fn write(&self, content: Vec<u8>) -> io::Result<()> {
        let directory = self.directory.clone();
        run_blocking(move || replace_file(&directory, &content)).await
    }
}

/// Runs `work`, which blocks, on tokio's blocking threads, so that it does not
/// hold up other asynchronous work. A panic in `work` reaches the caller as
/// the same panic. The error is for a task cancelled before it started, as can
/// happen when the runtime shuts down. Once started, the task cannot be
/// cancelled, so a caller has to wait for it.
async fn run_blocking(work: impl FnOnce() -> io::Result<()> + Send + 'static) -> io::Result<()> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Moving a complete temporary file over the old one is what makes the change
/// all or nothing. It is synced before the move so that a power failure cannot
/// leave the new name on content that never reached the disk. The directory is
/// not synced, so a power failure right after the move can undo the move. When
/// anything fails, dropping the temporary file removes it.
fn replace_file(directory: &Path, content: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!("{FILE_NAME}."))
        .tempfile_in(directory)?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(directory.join(FILE_NAME))
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn file_in(directory: &Path) -> SettingsFile {
        SettingsFile::in_directory(directory.to_path_buf())
    }

    fn names_in(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    // What each component puts directly in the data directory starts with the
    // component's name, so that the names of two components never collide.
    #[test]
    fn the_file_name_starts_with_the_name_of_the_component() {
        assert!(FILE_NAME.starts_with("config"));
    }

    #[tokio::test]
    #[should_panic(expected = "the work panicked")]
    async fn a_panic_in_the_blocking_work_reaches_the_caller() {
        let _ = run_blocking(|| panic!("the work panicked")).await;
    }

    #[tokio::test]
    async fn reading_finds_nothing_when_the_file_is_not_there() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(file_in(directory.path()).read().await.unwrap(), None);
    }

    #[tokio::test]
    async fn reading_finds_nothing_when_the_directory_is_not_there() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");

        assert_eq!(file_in(&missing).read().await.unwrap(), None);
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn reading_returns_what_was_written() {
        let directory = tempfile::tempdir().unwrap();
        let file = file_in(directory.path());

        file.write(b"content".to_vec()).await.unwrap();

        assert_eq!(file.read().await.unwrap(), Some(b"content".to_vec()));
    }

    #[tokio::test]
    async fn reading_fails_when_a_directory_is_in_the_place_of_the_file() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(FILE_NAME)).unwrap();

        assert!(file_in(directory.path()).read().await.is_err());
    }

    // On Windows, a path under a file is reported as not found.
    #[cfg(unix)]
    #[tokio::test]
    async fn reading_fails_when_a_file_is_in_the_place_of_the_directory() {
        let directory = tempfile::tempdir().unwrap();
        let not_a_directory = directory.path().join("file");
        std::fs::write(&not_a_directory, b"").unwrap();

        assert!(file_in(&not_a_directory).read().await.is_err());
    }

    #[tokio::test]
    async fn writing_makes_the_directories_that_are_not_there() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("a").join("b");
        let file = file_in(&nested);

        file.write(b"content".to_vec()).await.unwrap();

        assert_eq!(file.read().await.unwrap(), Some(b"content".to_vec()));
    }

    #[tokio::test]
    async fn writing_replaces_the_previous_content_entirely() {
        let directory = tempfile::tempdir().unwrap();
        let file = file_in(directory.path());
        file.write(b"a longer content".to_vec()).await.unwrap();

        file.write(b"short".to_vec()).await.unwrap();

        assert_eq!(file.read().await.unwrap(), Some(b"short".to_vec()));
    }

    #[tokio::test]
    async fn writing_leaves_only_the_settings_file_in_the_directory() {
        let directory = tempfile::tempdir().unwrap();

        file_in(directory.path())
            .write(b"content".to_vec())
            .await
            .unwrap();

        assert_eq!(names_in(directory.path()), [FILE_NAME]);
    }

    #[tokio::test]
    async fn writing_fails_when_a_file_is_in_the_place_of_the_directory() {
        let directory = tempfile::tempdir().unwrap();
        let not_a_directory = directory.path().join("file");
        std::fs::write(&not_a_directory, b"").unwrap();

        let result = file_in(&not_a_directory).write(b"content".to_vec()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_failed_write_leaves_no_temporary_file_behind() {
        let directory = tempfile::tempdir().unwrap();
        let in_the_way = directory.path().join(FILE_NAME);
        std::fs::create_dir(&in_the_way).unwrap();
        std::fs::write(in_the_way.join("inside"), b"").unwrap();

        let result = file_in(directory.path()).write(b"content".to_vec()).await;

        assert!(result.is_err());
        assert_eq!(names_in(directory.path()), [FILE_NAME]);
    }
}
