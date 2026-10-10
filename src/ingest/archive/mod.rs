//! Archive collections: list and read the documents inside a `.zip` /
//! `.tar` / `.tar.gz` / `.tgz` registered as a collection, without
//! extracting anything to disk.
//!
//! Reading is split into two passes so that an unchanged archive costs one
//! pass (a `.tar.gz` can only be listed by decompressing it from the start):
//!
//! 1. [`list_documents`] walks every entry once and keeps the documents mdya
//!    ingests, one per path. A later entry with the same path replaces an
//!    earlier one, as it would when the archive is extracted. A failure
//!    anywhere makes the whole archive unreadable, since a partial listing
//!    would make the writer delete the documents it failed to see. So does
//!    an archive whose mtime or size (its stamp, taken from the open file)
//!    changes during the walk: what was listed may be half old, half new.
//! 2. [`read_documents`] reads only the documents the writer asks for, in
//!    archive order, capping each at [`MAX_DOCUMENT_BYTES`] while it
//!    decompresses so a forged size in the archive cannot hide a zip bomb.
//!    A document that cannot be read fails on its own. The stamp must still
//!    be the listing's when the archive is opened and before each document
//!    is handed over; otherwise reading stops there, as it does for an
//!    archive that can no longer be read. An archive that keeps its stamp
//!    is taken to be the one listed, so the second pass gives the bytes
//!    whose CRCs the listing checked, including those of the gzip member it
//!    stops in without reaching that member's CRC.
//!
//! Entry names are checked in this order (see [`classify_name`]):
//!
//! 1. Entries that are not regular files are skipped: directories, links,
//!    devices and so on. Sparse files count as regular files. Both passes
//!    check this, in case the archive is replaced in between.
//! 2. Entries without an ingestable extension (`.md` / `.markdown` / `.pdf`)
//!    are skipped, judged on the raw bytes so a non-UTF-8 name of, say, an
//!    image is never counted.
//! 3. macOS AppleDouble metadata is skipped, again on the raw bytes, so a
//!    bad name in there is never counted either: entries under a top-level
//!    `__MACOSX/`, where Finder's zip puts it, and entries whose last
//!    segment starts with `._`, where macOS `tar` puts it next to the file
//!    it belongs to.
//! 4. The name must be valid UTF-8; otherwise the document is rejected.
//! 5. `\` is turned into `/`.
//! 6. Absolute names (leading `/` or a drive such as `C:`), `..` segments and
//!    control characters (NUL among them; see [`super::document_path`])
//!    reject the document. The check works on the string, not on `Path`, so
//!    the result does not depend on the OS.
//! 7. `.` and empty segments are dropped, so `./docs//a.md` is `docs/a.md`.
//!    The check in step 6 runs both before this, which would otherwise drop
//!    a leading `/`, and after it, which can bring a drive to the front
//!    (`./C:/a.md`).
//!
//! In steps 2 and 3, `\` only separates segments of a name that is valid
//! UTF-8, since in Shift_JIS byte `0x5C` can be half of a character. The
//! `__MACOSX` check is the exception: it only looks at the start of the
//! name, where a `0x5C` follows ASCII bytes and so is a real `\`.
//!
//! The names a listing keeps, together with the names being read for the
//! current entry, are capped at [`MAX_LISTED_NAME_BYTES`], counting
//! [`NAME_OVERHEAD`] more for each name: names compress so well that a
//! small `.tar.gz` could otherwise make the listing hold gigabytes of them.
//! While one name is read and checked, the buffer it is read into (a GNU
//! long name's grows as it is read, up to twice its length) and the
//! normalized copy the listing keeps are held at once, so for that moment
//! the names held can pass the cap by up to twice that one name.
//!
//! How each format is walked, and which of its forms make an archive
//! unreadable, is described in [`tar`], [`gzip`] and [`zip`].
//!
//! Paths always use `/`, on every OS, matching how the archive lists them.

mod gzip;
mod sparse;
mod tar;
#[cfg(test)]
mod tests;
mod zip;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::document_path::{
    NameProblem, RejectedDocument, escape_control_characters, has_control_character, shown_name,
};
use crate::format::FileFormat;
use gzip::GzipMembers;

/// Upper bound on one document's decompressed size. Ingest reads each
/// document into memory and holds the ones in progress
/// (`runtime.embed_parallelism` of them, or one when it is 0) plus two read
/// ahead, so with the default 8 their raw bytes take at most 2.5 GiB of the
/// default 8192 MB `runtime.memory_limit_mb`; extracted text and other
/// copies come on top of that.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = 256 * 1024 * 1024;

/// Upper bound on the names one listing holds, each counted with
/// [`NAME_OVERHEAD`] on top. A hundred-byte name leaves room for over a
/// million documents.
const MAX_LISTED_NAME_BYTES: u64 = 256 * 1024 * 1024;

/// What a listed name costs beyond its bytes: its allocation, its slot in
/// the listing's map and its position, roughly.
const NAME_OVERHEAD: u64 = 128;

/// Archive formats a collection path may point at, decided by extension
/// (case-insensitively), the same way document formats are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

impl ArchiveKind {
    /// `Some` when `path`'s file name ends in `.zip`, `.tar`, `.tar.gz` or
    /// `.tgz`, ignoring ASCII case.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }
        if name.ends_with(".tar") {
            return Some(Self::Tar);
        }
        name.ends_with(".zip").then_some(Self::Zip)
    }
}

/// What the listing makes of one entry name.
#[derive(Debug, PartialEq, Eq)]
enum EntryName {
    /// Not a document mdya ingests, or macOS metadata: skipped, not counted.
    Ignored,
    /// A document whose name cannot be used as a path: counted as failed.
    Rejected(NameProblem),
    /// A document, under the path it is indexed as.
    Document(PathBuf),
}

/// Apply the name rules from the module docs to one raw entry name.
fn classify_name(raw: &[u8]) -> EntryName {
    if !has_ingestable_extension(raw) || is_macos_metadata(raw) {
        return EntryName::Ignored;
    }
    let Ok(name) = std::str::from_utf8(raw) else {
        return EntryName::Rejected(NameProblem::NotUtf8);
    };
    // Checked before normalizing, which would drop a leading `/`, and again
    // after, which can bring a drive (`./C:/a.md`) to the front. Both treat
    // `\` as a separator rather than replacing it first: a name can be
    // hundreds of MiB long, so it is copied only into the normalized path.
    let normalized = normalize(name);
    if let Some(problem) = path_problem(name).or_else(|| path_problem(&normalized)) {
        return EntryName::Rejected(problem);
    }
    EntryName::Document(PathBuf::from(normalized))
}

/// The separators of a valid UTF-8 name: `/`, and `\` as Windows tools use.
const SEPARATORS: [char; 2] = ['/', '\\'];

/// Split a raw name into segments. `\` counts as a separator only when the
/// name is valid UTF-8: in a legacy encoding such as Shift_JIS, byte `0x5C`
/// can be the second half of a character.
fn segments(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    let backslash_separates = std::str::from_utf8(raw).is_ok();
    raw.split(move |&b| b == b'/' || (backslash_separates && b == b'\\'))
}

fn last_segment(raw: &[u8]) -> &[u8] {
    segments(raw).last().unwrap_or(raw)
}

/// Whether the last segment of `raw` has an extension mdya ingests. The
/// extension follows `Path::extension`: the part after the last `.`,
/// except for a name that starts with its only `.` (`.md` is a dotfile).
fn has_ingestable_extension(raw: &[u8]) -> bool {
    let file_name = last_segment(raw);
    let Some(dot) = file_name.iter().rposition(|&b| b == b'.') else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    std::str::from_utf8(&file_name[dot + 1..])
        .ok()
        .and_then(FileFormat::from_extension)
        .is_some()
}

/// Whether `raw` is macOS AppleDouble metadata: named `._…`, or under a
/// top-level `__MACOSX/` once `.` and empty segments are skipped the way
/// [`normalize`] drops them. Works on undecoded names.
fn is_macos_metadata(raw: &[u8]) -> bool {
    if last_segment(raw).starts_with(b"._") {
        return true;
    }
    let mut rest = raw
        .split(|&b| b == b'/' || b == b'\\')
        .filter(|s| !s.is_empty() && *s != b".");
    rest.next() == Some(b"__MACOSX".as_slice()) && rest.next().is_some()
}

fn path_problem(name: &str) -> Option<NameProblem> {
    if has_control_character(name) {
        return Some(NameProblem::ControlCharacter);
    }
    let bytes = name.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if name.starts_with(SEPARATORS) || has_drive {
        return Some(NameProblem::Absolute);
    }
    name.split(SEPARATORS)
        .any(|segment| segment == "..")
        .then_some(NameProblem::ParentDir)
}

/// `name` with `/` between its segments and without `.` and empty ones,
/// built without collecting the segments first (a long name can hold
/// millions of them). Its length is worked out first and allocated
/// exactly: the listing keeps the result and counts only its length
/// against the name limit, so it must not hold the spare room a growing
/// string would.
fn normalize(name: &str) -> String {
    let segments = || {
        name.split(SEPARATORS)
            .filter(|segment| !segment.is_empty() && *segment != ".")
    };
    let len = segments()
        .map(|segment| segment.len() + 1)
        .sum::<usize>()
        .saturating_sub(1);
    let mut normalized = String::with_capacity(len);
    for segment in segments() {
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(segment);
    }
    normalized
}

/// One document found by [`list_documents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveDocument {
    /// The path the document is indexed under.
    pub(crate) path: PathBuf,
    /// Where the entry sits in the archive (its number in a zip's central
    /// directory, or its count among a tar's entries), so
    /// [`read_documents`] can find it again.
    position: usize,
}

/// An archive file's mtime and size, as the open file reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArchiveStamp {
    modified: SystemTime,
    len: u64,
}

impl ArchiveStamp {
    fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            modified: metadata.modified()?,
            len: metadata.len(),
        })
    }

    /// The archive's mtime, which its documents are recorded with.
    pub(crate) fn modified(&self) -> SystemTime {
        self.modified
    }

    /// Fail unless `file` still has this stamp.
    fn check(&self, file: &File) -> io::Result<()> {
        if Self::of(file)? == *self {
            return Ok(());
        }
        Err(io::Error::other(
            "the archive's mtime or size changed while it was being read",
        ))
    }
}

/// The documents of one archive, from [`list_documents`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ArchiveListing {
    /// The archive's stamp from when it was opened to be listed, which it
    /// still had once the listing was read.
    pub(crate) stamp: ArchiveStamp,
    /// One per path, in archive order.
    pub(crate) documents: Vec<ArchiveDocument>,
    /// Document entries rejected for their names, in archive order.
    pub(crate) rejected: Vec<RejectedDocument>,
}

/// An archive that cannot be listed or read as a whole: missing,
/// unreadable, or corrupt part-way through.
#[derive(Debug, thiserror::Error)]
#[error("cannot read archive {}: {reason}", path.display())]
pub(crate) struct ArchiveError {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

impl ArchiveError {
    /// Control characters in `reason` are escaped, so nothing read from the
    /// archive can bring a line break or an escape sequence into the log.
    fn new(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_path_buf(),
            reason: escape_control_characters(&reason.to_string()),
        }
    }
}

/// Why one document could not be read out of an archive that otherwise
/// reads fine.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DocumentReadError {
    #[error("decompressed size exceeds the {} MiB limit", limit / (1024 * 1024))]
    TooLarge { limit: u64 },
    #[error("{0}")]
    Read(String),
    #[error("the archive changed while it was being read")]
    Changed,
}

/// An archive whose content breaks the format in a way that makes it
/// unreadable as a whole.
fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

/// One entry as the format-specific walks see it.
struct RawEntry<'a> {
    /// Where the entry comes in the archive, which decides the entry used
    /// among those with one path.
    order: u64,
    position: usize,
    name: &'a [u8],
    is_regular_file: bool,
}

/// A listing being built one entry at a time, so an entry that is not a
/// document is dropped before the next one is read: a small `.tar.gz` can
/// hold millions of them.
struct PartialListing {
    rejected: Vec<RejectedDocument>,
    /// The `(order, position)` of the last entry so far for each path.
    last: HashMap<PathBuf, (u64, usize)>,
    /// What the names kept so far cost, each with [`NAME_OVERHEAD`].
    name_bytes: u64,
    name_limit: u64,
}

impl PartialListing {
    fn new(name_limit: u64) -> Self {
        Self {
            rejected: Vec::new(),
            last: HashMap::new(),
            name_bytes: 0,
            name_limit,
        }
    }

    /// How many bytes of names the entry being read may still hold.
    fn name_room(&self) -> u64 {
        self.name_limit.saturating_sub(self.name_bytes)
    }

    /// Apply the name rules to one entry, keeping the last entry per path.
    /// Fails once the names kept pass the listing's limit.
    fn add(&mut self, entry: RawEntry<'_>) -> io::Result<()> {
        if !entry.is_regular_file {
            return Ok(());
        }
        match classify_name(entry.name) {
            EntryName::Ignored => {}
            EntryName::Rejected(problem) => {
                let name = shown_name(entry.name);
                self.charge(name.len())?;
                self.rejected.push(RejectedDocument { name, problem });
            }
            EntryName::Document(path) => {
                let latest = (entry.order, entry.position);
                match self.last.entry(path) {
                    Entry::Occupied(mut kept) => {
                        let kept = kept.get_mut();
                        *kept = (*kept).max(latest);
                    }
                    Entry::Vacant(slot) => {
                        let cost = slot.key().as_os_str().len();
                        slot.insert(latest);
                        self.charge(cost)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn charge(&mut self, name_len: usize) -> io::Result<()> {
        self.name_bytes += name_len as u64 + NAME_OVERHEAD;
        if self.name_bytes > self.name_limit {
            return Err(names_too_large(self.name_limit));
        }
        Ok(())
    }

    /// The listing, in archive order.
    fn finish(self, stamp: ArchiveStamp) -> ArchiveListing {
        let mut documents: Vec<(u64, ArchiveDocument)> = self
            .last
            .into_iter()
            .map(|(path, (order, position))| (order, ArchiveDocument { path, position }))
            .collect();
        documents.sort_by_key(|(order, _)| *order);
        ArchiveListing {
            stamp,
            documents: documents
                .into_iter()
                .map(|(_, document)| document)
                .collect(),
            rejected: self.rejected,
        }
    }
}

fn names_too_large(limit: u64) -> io::Error {
    invalid(format!(
        "the names of the documents in the archive exceed {} MiB",
        limit / (1024 * 1024)
    ))
}

/// List the documents in `archive`. A failure anywhere, including part-way
/// through a truncated archive, is an [`ArchiveError`]: a partial listing
/// would make the writer delete the documents it failed to see.
pub(crate) fn list_documents(
    archive: &Path,
    kind: ArchiveKind,
) -> Result<ArchiveListing, ArchiveError> {
    list_documents_limited(archive, kind, MAX_LISTED_NAME_BYTES)
}

/// [`list_documents`] with the name limit as a parameter, so tests can
/// exercise it without hundreds of MiB of names.
fn list_documents_limited(
    archive: &Path,
    kind: ArchiveKind,
    name_limit: u64,
) -> Result<ArchiveListing, ArchiveError> {
    File::open(archive)
        .and_then(|file| list_unchanged(&file, &file, kind, name_limit))
        .map_err(|reason| ArchiveError::new(archive, reason))
}

/// List the archive `file` holds, read through `reader`. The stamp is taken
/// from the open file before the walk, so the mtime recorded is that of the
/// file listed, and checked again after it, so a listing read from an
/// archive rewritten meanwhile is never used. (`reader` is apart from
/// `file` only so tests can rewrite the file part-way through the walk.)
fn list_unchanged(
    file: &File,
    reader: impl Read + Seek,
    kind: ArchiveKind,
    name_limit: u64,
) -> io::Result<ArchiveListing> {
    let stamp = ArchiveStamp::of(file)?;
    let mut listing = PartialListing::new(name_limit);
    let reader = BufReader::new(reader);
    match kind {
        ArchiveKind::Zip => zip::list_zip(reader, &mut listing)?,
        ArchiveKind::Tar => drop(tar::list_tar(reader, &mut listing)?),
        // The member holding the end of the tar is read to its end, so its
        // CRC is checked; nothing after that member is read.
        ArchiveKind::TarGz => {
            tar::list_tar(BufReader::new(GzipMembers::new(reader)), &mut listing)?
                .get_mut()
                .finish_member()?
        }
    }
    stamp.check(file)?;
    Ok(listing.finish(stamp))
}

/// Read `wanted` (from [`list_documents`], in archive order) out of
/// `archive` and hand each one to `deliver`, which returns `false` to stop
/// early. A document that cannot be read is delivered as an error; an
/// archive that cannot be read at all returns [`ArchiveError`], and the
/// documents not delivered by then are the caller's to account for.
///
/// The open archive must still have `stamp`, the listing's, when it is
/// opened and each time a document has been read and is about to be handed
/// over; otherwise reading stops there, with that document not handed over,
/// as for an archive that cannot be read.
pub(crate) fn read_documents(
    archive: &Path,
    kind: ArchiveKind,
    stamp: ArchiveStamp,
    wanted: &[ArchiveDocument],
    deliver: impl FnMut(&ArchiveDocument, Result<Vec<u8>, DocumentReadError>) -> bool,
) -> Result<(), ArchiveError> {
    read_documents_capped(archive, kind, stamp, wanted, MAX_DOCUMENT_BYTES, deliver)
}

/// [`read_documents`] with the size cap as a parameter, so tests can
/// exercise it without 256 MiB entries.
fn read_documents_capped(
    archive: &Path,
    kind: ArchiveKind,
    stamp: ArchiveStamp,
    wanted: &[ArchiveDocument],
    limit: u64,
    mut deliver: impl FnMut(&ArchiveDocument, Result<Vec<u8>, DocumentReadError>) -> bool,
) -> Result<(), ArchiveError> {
    if wanted.is_empty() {
        return Ok(());
    }
    let result = File::open(archive).and_then(|file| {
        stamp.check(&file)?;
        let mut changed = None;
        let deliver_unchanged = |document: &ArchiveDocument, read| match stamp.check(&file) {
            Ok(()) => deliver(document, read),
            Err(e) => {
                changed = Some(e);
                false
            }
        };
        let reader = BufReader::new(&file);
        match kind {
            ArchiveKind::Zip => zip::read_zip(reader, wanted, limit, deliver_unchanged)?,
            ArchiveKind::Tar => tar::read_tar(reader, wanted, limit, deliver_unchanged)?,
            ArchiveKind::TarGz => tar::read_tar(
                BufReader::new(GzipMembers::new(reader)),
                wanted,
                limit,
                deliver_unchanged,
            )?,
        }
        changed.map_or(Ok(()), Err)
    });
    result.map_err(|reason| ArchiveError::new(archive, reason))
}

/// Whether the raw entry name still yields `path`, so a document is never
/// read from an entry that moved because the archive was replaced between
/// the two passes.
fn names(raw: &[u8], path: &Path) -> bool {
    matches!(classify_name(raw), EntryName::Document(p) if p == path)
}

/// Read a whole entry, failing once it passes `limit` bytes. At most `limit`
/// bytes are kept, and one more is read only to tell whether the entry goes
/// on: reading `limit + 1` into the buffer would grow it to twice the limit
/// before failing.
fn read_capped(mut entry: impl Read, limit: u64) -> Result<Vec<u8>, DocumentReadError> {
    let read_error = |e: io::Error| DocumentReadError::Read(e.to_string());
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if bytes.len() as u64 == limit && entry.read(&mut [0; 1]).map_err(read_error)? > 0 {
        return Err(DocumentReadError::TooLarge { limit });
    }
    Ok(bytes)
}
