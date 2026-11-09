//! What the driver needs from the outside of its own logic: the zip and tar
//! readers, and duplicating the open archive. Tests replace all of it.

use std::fs::File;
use std::io;

use super::kind::ArchiveKind;
use super::tar::{tar_contents, tar_listing};
use super::transfer::{Contents, Listing};
use super::zip::{zip_contents, zip_listing};
use crate::types::BoxError;

pub(super) trait Readers: Send + Sync {
    fn clone_file(&self, file: &File) -> io::Result<File>;
    fn listing(&self, kind: ArchiveKind, file: File) -> Box<dyn Listing>;
    fn contents(
        &self,
        kind: ArchiveKind,
        file: File,
        max_content: u64,
    ) -> Result<Box<dyn Contents>, BoxError>;
}

/// The zip and tar readers of this component.
pub(super) struct RealReaders;

impl Readers for RealReaders {
    fn clone_file(&self, file: &File) -> io::Result<File> {
        file.try_clone()
    }

    fn listing(&self, kind: ArchiveKind, file: File) -> Box<dyn Listing> {
        match kind {
            ArchiveKind::Zip => zip_listing(file),
            ArchiveKind::Tar => tar_listing(file, false),
            ArchiveKind::GzippedTar => tar_listing(file, true),
        }
    }

    fn contents(
        &self,
        kind: ArchiveKind,
        file: File,
        max_content: u64,
    ) -> Result<Box<dyn Contents>, BoxError> {
        match kind {
            ArchiveKind::Zip => zip_contents(file, max_content),
            ArchiveKind::Tar => tar_contents(file, false, max_content),
            ArchiveKind::GzippedTar => tar_contents(file, true, max_content),
        }
    }
}
