//! Per-collection orphan cleanup.
//!
//! "Orphan" = a `(collection, path)` row in the `chunks` table whose
//! file no longer exists on disk. The user removed / renamed the file
//! between `mdya update-all` runs; we delete the now-pointless chunks
//! so search results stay accurate.
//!
//! Algorithm: full scan diff. The walker has already produced the
//! current filesystem set; we ask the DB for its set and delete what
//! is in DB but not on disk. `(collection, path)` is the unique key
//! so per-path delete is correct.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Compute paths that should be deleted from the DB for one collection.
/// Both inputs are relative paths (collection-rooted).
pub fn compute_orphans(fs_paths: &BTreeSet<PathBuf>, db_paths: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    db_paths.difference(fs_paths).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set<I: IntoIterator<Item = &'static str>>(items: I) -> BTreeSet<PathBuf> {
        items.into_iter().map(PathBuf::from).collect()
    }

    #[test]
    fn empty_fs_returns_every_db_path_as_orphan() {
        let fs = set([]);
        let db = set(["a.md", "b.md"]);
        let mut got = compute_orphans(&fs, &db);
        got.sort();
        assert_eq!(got, vec![PathBuf::from("a.md"), PathBuf::from("b.md")]);
    }

    #[test]
    fn full_overlap_returns_no_orphan() {
        let fs = set(["a.md", "b.md"]);
        let db = set(["a.md", "b.md"]);
        assert!(compute_orphans(&fs, &db).is_empty());
    }

    #[test]
    fn db_minus_fs_is_orphan_fs_minus_db_is_not() {
        // user removed `removed.md` and added `new.md` since last ingest.
        let fs = set(["kept.md", "new.md"]);
        let db = set(["kept.md", "removed.md"]);
        let got = compute_orphans(&fs, &db);
        assert_eq!(got, vec![PathBuf::from("removed.md")]);
    }

    #[test]
    fn empty_db_returns_no_orphan_even_when_fs_has_files() {
        let fs = set(["a.md"]);
        let db = set([]);
        assert!(compute_orphans(&fs, &db).is_empty());
    }
}
