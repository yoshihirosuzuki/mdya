//! Handing the content of the files to the shared ingest flow.

use std::fs;
use std::path::{Path, PathBuf};
use std::vec;

use async_trait::async_trait;

use super::blocking::run_blocking;
use crate::ingest::ItemContents;
use crate::types::BoxError;

#[derive(Debug, thiserror::Error)]
#[error("the place is no longer a regular file")]
struct NotAFileAnymore;

/// Reads the files of the items with a key, one at a time when asked, so that
/// what is in memory stays within what the flow has asked for.
pub(super) struct FileContents {
    files: vec::IntoIter<PathBuf>,
}

impl FileContents {
    /// `files` are where the content of the items is, in the order of the
    /// items.
    pub(super) fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files: files.into_iter(),
        }
    }
}

#[async_trait]
impl ItemContents for FileContents {
    async fn next(&mut self) -> Option<Result<Vec<u8>, BoxError>> {
        let file = self.files.next()?;
        Some(run_blocking(move || read_regular_file(&file)).await)
    }
}

/// What is at the place now, as long as it is still a regular file: an edit
/// since the walk is fine, but a link must not be followed, and a named pipe
/// must not be waited on.
fn read_regular_file(file: &Path) -> Result<Vec<u8>, BoxError> {
    if !fs::symlink_metadata(file)?.is_file() {
        return Err(Box::new(NotAFileAnymore));
    }
    Ok(fs::read(file)?)
}

#[cfg(test)]
mod tests {
    use std::io;

    use tempfile::TempDir;

    use super::*;

    fn directory_with(names: &[&str]) -> (TempDir, Vec<PathBuf>) {
        let directory = tempfile::tempdir().unwrap();
        let files = names
            .iter()
            .map(|name| {
                let file = directory.path().join(name);
                fs::write(&file, name.as_bytes()).unwrap();
                file
            })
            .collect();
        (directory, files)
    }

    #[tokio::test]
    async fn the_contents_come_in_the_order_of_the_items_and_then_end() {
        let (_directory, files) = directory_with(&["b.md", "a.md"]);
        let mut contents = FileContents::new(files);

        assert_eq!(contents.next().await.unwrap().unwrap(), b"b.md");
        assert_eq!(contents.next().await.unwrap().unwrap(), b"a.md");
        assert!(contents.next().await.is_none());
    }

    #[tokio::test]
    async fn a_file_that_changed_since_the_walk_hands_over_what_it_holds_now() {
        let (_directory, files) = directory_with(&["a.md"]);
        fs::write(&files[0], b"edited").unwrap();
        let mut contents = FileContents::new(files);

        assert_eq!(contents.next().await.unwrap().unwrap(), b"edited");
    }

    #[tokio::test]
    async fn a_file_that_is_gone_is_an_error_and_the_next_one_is_still_read() {
        let (_directory, files) = directory_with(&["a.md", "b.md"]);
        fs::remove_file(&files[0]).unwrap();
        let mut contents = FileContents::new(files);

        let cause = contents.next().await.unwrap().unwrap_err();

        let cause = cause.downcast_ref::<io::Error>().unwrap();
        assert_eq!(cause.kind(), io::ErrorKind::NotFound);
        assert_eq!(contents.next().await.unwrap().unwrap(), b"b.md");
    }

    #[tokio::test]
    async fn a_directory_in_the_place_of_the_file_is_an_error() {
        let (_directory, files) = directory_with(&["a.md"]);
        fs::remove_file(&files[0]).unwrap();
        fs::create_dir(&files[0]).unwrap();
        let mut contents = FileContents::new(files);

        let cause = contents.next().await.unwrap().unwrap_err();

        assert!(cause.downcast_ref::<NotAFileAnymore>().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_in_the_place_of_the_file_is_an_error_and_is_not_followed() {
        use std::os::unix::fs::symlink;

        let (directory, files) = directory_with(&["a.md"]);
        let outside = directory.path().join("outside");
        fs::write(&outside, b"not to be read").unwrap();
        fs::remove_file(&files[0]).unwrap();
        symlink(&outside, &files[0]).unwrap();
        let mut contents = FileContents::new(files);

        let cause = contents.next().await.unwrap().unwrap_err();

        assert!(cause.downcast_ref::<NotAFileAnymore>().is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_an_error() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let (_directory, files) = directory_with(&["a.md"]);
        fs::set_permissions(&files[0], Permissions::from_mode(0o000)).unwrap();
        let superuser_reads_anything = fs::read(&files[0]).is_ok();
        let mut contents = FileContents::new(files);

        let result = contents.next().await.unwrap();

        if !superuser_reads_anything {
            let cause = result.unwrap_err();
            let cause = cause.downcast_ref::<io::Error>().unwrap();
            assert_eq!(cause.kind(), io::ErrorKind::PermissionDenied);
        }
    }
}
