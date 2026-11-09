//! Which reader an archive's name calls for.

use std::path::Path;

/// The formats of archive that mdya reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArchiveKind {
    Zip,
    Tar,
    GzippedTar,
}

impl ArchiveKind {
    /// The format that the extension of a location's own name stands for,
    /// whatever case it is in. A symbolic link is judged by its own name.
    pub(super) fn of_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".zip") {
            return Some(Self::Zip);
        }
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return Some(Self::GzippedTar);
        }
        name.ends_with(".tar").then_some(Self::Tar)
    }
}
