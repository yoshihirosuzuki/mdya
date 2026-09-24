//! File walker for the formats mdya ingests.
//!
//! Extension judgement is delegated to [`FileFormat::from_path`]
//! (`crate::format`) so the set of ingestable extensions lives in one
//! place — the walker itself has no extension constants to keep in sync
//! with the writer's dispatch.
//!
//! `.gitignore` policy: when `root` is inside a git repository (a `.git`
//! directory or file — or a Jujutsu `.jj` directory — at `root` or above
//! it), paths that `.gitignore` rules exclude are not visited — including
//! rules from `.gitignore` files above `root`. That is the only exclusion
//! source: no `.git/info/exclude`, no global excludes file, no `.ignore`
//! files, and hidden entries (`.git/`, `.github/`, `.claude/`) are still
//! walked unless a rule excludes them. Because `.git/info/exclude` is not
//! read, moving a rule there keeps git ignoring the path while mdya
//! indexes it. Rules from parent directories are never applied to `root`
//! itself but still apply to the entries below it, so registering an
//! ignored directory as its own collection only helps when the rule
//! names the directory (`notes/`), not when it matches its contents
//! (`notes/*`, `*.pdf`).
//!
//! Symlink policy: `.follow_links(false)` (the `ignore` default) —
//! symlinks inside the collection tree are not traversed (cycle /
//! root-escape / size-explosion risks are eliminated by construction).
//! `ignore` leaves `walkdir`'s `follow_root_links` default of `true`
//! untouched, so a collection whose root itself is a symlink (e.g.
//! `~/notes -> ~/Dropbox/notes`) still has its target directory walked.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

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
    WalkBuilder::new(root)
        .hidden(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
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

    /// Mark `dir` as a git repository root. An empty `.git/` directory is
    /// enough: the walker only checks for its presence, never its contents.
    fn mark_git_repo(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    }

    fn write_gitignore(dir: &Path, rules: &str) {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(dir.join(".gitignore"), rules).expect("write .gitignore");
    }

    fn relative_paths(root: &Path) -> Vec<PathBuf> {
        let mut got = collect_ingestable_files(root);
        got.sort();
        got
    }

    /// The "no repository around" tests assume the temp directory is not
    /// itself inside a git / jj working tree; a developer with `TMPDIR`
    /// pointing into a checkout would otherwise see the ancestor's rules
    /// applied. Such environments skip those tests instead of failing.
    fn temp_dir_is_inside_a_repository(dir: &Path) -> bool {
        dir.ancestors()
            .skip(1)
            .any(|ancestor| ancestor.join(".git").exists() || ancestor.join(".jj").exists())
    }

    #[test]
    fn hidden_directories_are_walked_when_nothing_ignores_them() {
        // Hidden entries (`.git/`, `.claude/`, `.github/`) are not skipped by
        // name; only `.gitignore` rules exclude paths, and this root has none.
        let tmp = TempDir::new().expect("tempdir");
        if temp_dir_is_inside_a_repository(tmp.path()) {
            eprintln!("skipping: temp dir is inside a repository");
            return;
        }
        touch(tmp.path(), ".git/INSIDE.md");
        touch(tmp.path(), ".claude/rules/style.md");
        touch(tmp.path(), "node_modules/pkg/README.md");
        let got = collect_ingestable_files(tmp.path());
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn gitignored_paths_are_excluded_inside_a_git_repository() {
        let tmp = TempDir::new().expect("tempdir");
        mark_git_repo(tmp.path());
        write_gitignore(tmp.path(), ".worktree/\nnode_modules/\ntmp/\n");
        touch(tmp.path(), "kept.md");
        touch(tmp.path(), ".worktree/scratch/notes.md");
        touch(tmp.path(), "node_modules/pkg/README.md");
        touch(tmp.path(), "tmp/draft.md");

        assert_eq!(relative_paths(tmp.path()), vec![PathBuf::from("kept.md")]);
    }

    #[test]
    fn gitignore_is_read_when_dot_git_is_a_file() {
        // A git worktree carries a `.git` *file* (a `gitdir:` pointer)
        // rather than a directory; the walker must treat it as a repository
        // marker too.
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join(".git"), b"gitdir: /nowhere\n").expect("write .git");
        write_gitignore(tmp.path(), "build/\n");
        touch(tmp.path(), "kept.md");
        touch(tmp.path(), "build/generated.md");

        assert_eq!(relative_paths(tmp.path()), vec![PathBuf::from("kept.md")]);
    }

    #[test]
    fn gitignore_in_a_subdirectory_applies_to_that_subtree() {
        let tmp = TempDir::new().expect("tempdir");
        mark_git_repo(tmp.path());
        write_gitignore(&tmp.path().join("docs"), "generated/\n");
        touch(tmp.path(), "docs/index.md");
        touch(tmp.path(), "docs/generated/api.md");
        touch(tmp.path(), "generated/elsewhere.md");

        assert_eq!(
            relative_paths(tmp.path()),
            vec![
                PathBuf::from("docs/index.md"),
                PathBuf::from("generated/elsewhere.md"),
            ]
        );
    }

    #[test]
    fn parent_gitignore_applies_when_root_is_a_subdirectory_of_the_repository() {
        let tmp = TempDir::new().expect("tempdir");
        mark_git_repo(tmp.path());
        write_gitignore(tmp.path(), "build/\n");
        touch(tmp.path(), "docs/guide.md");
        touch(tmp.path(), "docs/build/guide.md");

        let root = tmp.path().join("docs");
        assert_eq!(relative_paths(&root), vec![PathBuf::from("guide.md")]);
    }

    #[test]
    fn gitignored_directory_registered_as_root_is_still_walked() {
        // Parent rules are not applied to the root itself, so a rule that
        // names the directory (`notes/`) stops mattering once `notes` is a
        // collection root of its own. This is the escape hatch the manual
        // documents for directory-name rules.
        let tmp = TempDir::new().expect("tempdir");
        mark_git_repo(tmp.path());
        write_gitignore(tmp.path(), "notes/\n");
        touch(tmp.path(), "notes/private.md");

        let root = tmp.path().join("notes");
        assert_eq!(relative_paths(&root), vec![PathBuf::from("private.md")]);
    }

    #[test]
    fn parent_rules_matching_the_contents_still_apply_when_ignored_directory_is_root() {
        // The counterpart of the test above: parent rules do apply to the
        // entries below the root, so `drafts/*` and `*.pdf` keep excluding
        // files even when `drafts` is registered as its own collection. The
        // manual points such users at `.git/info/exclude` instead.
        let tmp = TempDir::new().expect("tempdir");
        mark_git_repo(tmp.path());
        write_gitignore(tmp.path(), "drafts/*\n*.pdf\n");
        touch(tmp.path(), "drafts/idea.md");
        touch(tmp.path(), "papers/paper.pdf");
        touch(tmp.path(), "papers/summary.md");

        assert!(relative_paths(&tmp.path().join("drafts")).is_empty());
        assert_eq!(
            relative_paths(&tmp.path().join("papers")),
            vec![PathBuf::from("summary.md")]
        );
    }

    #[test]
    fn gitignore_is_not_read_outside_a_git_repository() {
        // Matches git itself: a stray `.gitignore` in a plain directory has
        // no effect.
        let tmp = TempDir::new().expect("tempdir");
        if temp_dir_is_inside_a_repository(tmp.path()) {
            eprintln!("skipping: temp dir is inside a repository");
            return;
        }
        write_gitignore(tmp.path(), "ignored/\n");
        touch(tmp.path(), "kept.md");
        touch(tmp.path(), "ignored/also-kept.md");

        assert_eq!(
            relative_paths(tmp.path()),
            vec![
                PathBuf::from("ignored/also-kept.md"),
                PathBuf::from("kept.md")
            ]
        );
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
