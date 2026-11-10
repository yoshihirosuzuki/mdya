//! Walking the directory of a collection to find its documents.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};

use super::causes;
use super::names::{extension_of, key_of, shown_name};
use crate::format::Format;
use crate::ingest::{IngestItem, IngestItems};
use crate::types::{BoxError, Time};

/// What a walk found.
pub(super) struct Walked {
    pub(super) items: IngestItems,
    /// Where the content of each item with a key is, in the order of the
    /// items.
    pub(super) files: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("the path is not valid UTF-8, so the document has no key")]
struct NameNotUtf8;

/// Finds the documents under `root`. Fails only when `root` itself cannot be
/// read. A place under it that cannot be read is warned about and counted, and
/// the walk goes on.
pub(super) fn walk(root: &Path, collection: &str, format: &dyn Format) -> io::Result<Walked> {
    fs::read_dir(root)?;
    let mut state = Walk::new(root, collection, format);
    for entry in walker(root) {
        state.take(entry);
    }
    Ok(state.finish())
}

/// Links are not followed, except a link at `root` itself. The `.gitignore`
/// rules apply only when `root` is in a repository, also the rules above
/// `root`, which do not apply to `root` itself. No other file of rules is
/// read. A file of rules that cannot be read is passed over by the walker
/// without an error.
fn walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(in_repository(root))
        .git_global(false)
        .git_exclude(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build()
}

/// Whether the directory `root` leads to, or a directory above it, has a
/// `.git` (a directory or a file) or a `.jj` directory. The walker's own check
/// would also take a `.jj` file, and a repository below `root`.
fn in_repository(root: &Path) -> bool {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    root.ancestors()
        .any(|dir| dir.join(".git").exists() || dir.join(".jj").is_dir())
}

struct Walk<'a> {
    root: &'a Path,
    collection: &'a str,
    format: &'a dyn Format,
    items: Vec<IngestItem>,
    files: Vec<PathBuf>,
    unreadable_without_key: Vec<(String, BoxError)>,
    unreadable_dirs: Vec<String>,
    unreadable_places: u64,
    /// False once a place could not be read and its documents cannot be told
    /// from those of the other places, so that none may be deleted.
    can_delete: bool,
}

impl<'a> Walk<'a> {
    fn new(root: &'a Path, collection: &'a str, format: &'a dyn Format) -> Self {
        Self {
            root,
            collection,
            format,
            items: Vec::new(),
            files: Vec::new(),
            unreadable_without_key: Vec::new(),
            unreadable_dirs: Vec::new(),
            unreadable_places: 0,
            can_delete: true,
        }
    }

    fn take(&mut self, entry: Result<DirEntry, ignore::Error>) {
        match entry {
            Ok(entry) => self.take_entry(&entry),
            Err(error) => self.take_error(&error),
        }
    }

    fn take_entry(&mut self, entry: &DirEntry) {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            return;
        }
        let Some(file_type) = extension_of(entry.file_name())
            .and_then(|extension| self.format.file_type_from_extension(extension))
        else {
            return;
        };
        let relative = self.relative(entry.path());
        let Some(key) = key_of(relative) else {
            let name = shown_name(relative);
            self.unreadable_without_key
                .push((name, Box::new(NameNotUtf8)));
            return;
        };
        match modified(entry) {
            Ok(time) => {
                self.items.push(IngestItem {
                    key,
                    time,
                    file_type,
                });
                self.files.push(entry.path().to_path_buf());
            }
            Err(cause) => {
                self.can_delete = false;
                self.note_unreadable_place(&shown_name(relative), cause.as_ref());
            }
        }
    }

    fn take_error(&mut self, error: &ignore::Error) {
        if !is_read_failure(error) {
            return;
        }
        let place = place_of(error);
        match self.unreadable_directory(place) {
            Some(relative) => self
                .unreadable_dirs
                .extend(key_of(relative).map(|key| key.0)),
            None => self.can_delete = false,
        }
        self.note_unreadable_place(&self.shown_place(place), error);
    }

    /// The place as a path from `root`, when it is a directory under `root`.
    /// Only then can the documents that were under it be told by their keys.
    fn unreadable_directory<'e>(&self, place: Option<&'e Path>) -> Option<&'e Path> {
        let place = place?;
        let relative = place.strip_prefix(self.root).ok()?;
        let is_a_directory = fs::symlink_metadata(place).is_ok_and(|metadata| metadata.is_dir());
        (is_a_directory && !relative.as_os_str().is_empty()).then_some(relative)
    }

    fn shown_place(&self, place: Option<&Path>) -> String {
        let Some(place) = place else {
            return "unknown".to_owned();
        };
        match place.strip_prefix(self.root) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
            Ok(relative) => shown_name(relative),
            Err(_) => shown_name(place),
        }
    }

    fn note_unreadable_place(&mut self, place: &str, cause: &(dyn Error + 'static)) {
        self.unreadable_places += 1;
        tracing::warn!(
            collection = self.collection,
            place,
            cause = causes(cause).as_str(),
            "a part of the directory cannot be read and was skipped"
        );
    }

    #[expect(clippy::expect_used)]
    fn relative<'e>(&self, path: &'e Path) -> &'e Path {
        path.strip_prefix(self.root)
            .expect("the walker makes every path by joining names onto the root it was given")
    }

    fn finish(self) -> Walked {
        Walked {
            items: IngestItems {
                items: self.items,
                unreadable_without_key: self.unreadable_without_key,
                delete_except_unreadable_dirs: self.can_delete.then_some(self.unreadable_dirs),
                unreadable_places: self.unreadable_places,
            },
            files: self.files,
        }
    }
}

fn modified(entry: &DirEntry) -> Result<Time, BoxError> {
    Ok(entry.metadata()?.modified()?)
}

/// Whether the error says that a place could not be read, rather than that a
/// rule in a `.gitignore` could not be understood.
fn is_read_failure(error: &ignore::Error) -> bool {
    match error {
        ignore::Error::Partial(errors) => errors.iter().any(is_read_failure),
        ignore::Error::WithLineNumber { err, .. }
        | ignore::Error::WithPath { err, .. }
        | ignore::Error::WithDepth { err, .. } => is_read_failure(err),
        ignore::Error::Io(_) | ignore::Error::Loop { .. } => true,
        ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => false,
    }
}

/// Where the error happened, when it says so. An entry that could not be read
/// from a directory partway through gives an error without a place.
fn place_of(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            place_of(err)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
