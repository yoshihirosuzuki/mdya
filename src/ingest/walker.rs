//! File walker for the formats mdya ingests.
//!
//! Extension judgement is delegated to [`FileFormat::from_path`]
//! (`crate::format`) so the set of ingestable extensions lives in one
//! place — the walker itself has no extension constants to keep in sync
//! with the writer's dispatch.
//!
//! Skip rules remain zero (no `.git/` / `node_modules/` exclusions):
//! the walker visits every directory under `root` and emits
//! only files whose extension `FileFormat::from_path` recognises.
//! User-facing collections rooted at directories that contain many
//! non-ingestable files are still cheap because we only collect
//! matching paths.
//!
//! Symlink policy: `.follow_links(false)` — symlinks inside
//! the collection tree are not traversed (cycle / root-escape / size-
//! explosion risks are eliminated by construction). The `walkdir`
//! `follow_root_links` default of `true` is kept untouched so a
//! collection whose root itself is a symlink (e.g.
//! `~/notes -> ~/Dropbox/notes`) still has its target directory walked.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::format::FileFormat;

/// Recursively walk `root` and return every file whose extension is one
/// mdya ingests (currently `.md` / `.markdown` / `.pdf`, decided by
/// [`FileFormat::from_path`]). Paths are returned **relative to `root`**,
/// matching the `chunks.path` column convention.
///
/// Returning a `Vec` rather than an iterator is deliberate: the orphan
/// step (`super::orphan`) consumes the same path set, and constructing
/// it twice is more expensive than the memory of holding it once
/// (typical personal-note collection: 100s–1000s of paths, each ~50
/// bytes).
pub fn collect_ingestable_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| FileFormat::from_path(entry.path()).is_some())
        .filter_map(|entry| relative_to(root, entry.path()))
        .collect()
}

fn relative_to(root: &Path, absolute: &Path) -> Option<PathBuf> {
    absolute.strip_prefix(root).ok().map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, b"x").expect("write");
    }

    #[test]
    fn empty_root_returns_no_files() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(collect_ingestable_files(tmp.path()).is_empty());
    }

    #[test]
    fn ingestable_extensions_include_md_markdown_and_pdf() {
        let tmp = TempDir::new().expect("tempdir");
        touch(tmp.path(), "a.md");
        touch(tmp.path(), "b.MARKDOWN");
        touch(tmp.path(), "c.txt");
        touch(tmp.path(), "d.rs");
        touch(tmp.path(), "e.png");
        touch(tmp.path(), "f.pdf");
        touch(tmp.path(), "g.PDF");

        let mut got: Vec<String> = collect_ingestable_files(tmp.path())
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "a.md".to_string(),
                "b.MARKDOWN".to_string(),
                "f.pdf".to_string(),
                "g.PDF".to_string(),
            ]
        );
    }

    #[test]
    fn nested_directories_walked_recursively_for_all_formats() {
        let tmp = TempDir::new().expect("tempdir");
        touch(tmp.path(), "top.md");
        touch(tmp.path(), "sub/inner.md");
        touch(tmp.path(), "sub/deep/leaf.pdf");

        let got: Vec<String> = collect_ingestable_files(tmp.path())
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got.len(), 3);
        // Path separator normalisation: on Windows the strings would use `\`,
        // so check membership via the platform Path API instead of exact
        // string equality.
        let got_paths: Vec<PathBuf> = got.iter().map(PathBuf::from).collect();
        assert!(got_paths.contains(&PathBuf::from("top.md")));
        assert!(got_paths.contains(&PathBuf::from("sub/inner.md")));
        assert!(got_paths.contains(&PathBuf::from("sub/deep/leaf.pdf")));
    }

    #[test]
    fn hidden_directories_are_walked_no_skip_rules() {
        // The walker has no skip patterns for `.git/` / `node_modules/` etc.,
        // so a `.git/COMMIT_EDITMSG.md` under a collection root will be
        // picked up; the trade-off is that users must point collections at
        // clean directories.
        let tmp = TempDir::new().expect("tempdir");
        touch(tmp.path(), ".git/INSIDE.md");
        touch(tmp.path(), "node_modules/pkg/README.md");
        let got = collect_ingestable_files(tmp.path());
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn directories_with_ingestable_extension_are_not_returned() {
        let tmp = TempDir::new().expect("tempdir");
        fs::create_dir_all(tmp.path().join("weird.md")).expect("mkdir");
        touch(tmp.path(), "weird.md/inner.md");

        let got = collect_ingestable_files(tmp.path());
        let got_paths: Vec<PathBuf> = got.iter().map(|p| p.to_path_buf()).collect();
        assert_eq!(got_paths, vec![PathBuf::from("weird.md/inner.md")]);
    }
}
