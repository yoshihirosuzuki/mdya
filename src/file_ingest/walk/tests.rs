use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use tracing::Level;

use super::*;
use crate::format::FileType;
use crate::format::test_double::TestFormat;
use crate::test_support::log_receiver::{LogRecord, capture};

fn format() -> TestFormat {
    TestFormat::new()
        .with_extension("md", "markdown")
        .with_extension("pdf", "pdf")
}

fn touch(root: &Path, relative: &str) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, b"x").unwrap();
}

fn walked(root: &Path) -> Walked {
    walk(root, "notes", &format()).unwrap()
}

fn keys(walked: &Walked) -> Vec<&str> {
    walked
        .items
        .items
        .iter()
        .map(|item| item.key.0.as_str())
        .collect()
}

fn field<'a>(record: &'a LogRecord, name: &str) -> &'a str {
    record
        .fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.as_str())
        .unwrap()
}

fn mark_git_repository(dir: &Path) {
    fs::create_dir_all(dir.join(".git")).unwrap();
}

/// The tests without a repository around assume the temporary directory is not
/// in one. With `TMPDIR` inside a checkout, they have nothing to check.
fn is_in_a_repository(dir: &Path) -> bool {
    dir.ancestors()
        .skip(1)
        .any(|ancestor| ancestor.join(".git").exists() || ancestor.join(".jj").exists())
}

// What becomes a document, and what it holds.

#[test]
fn documents_are_found_at_any_depth_with_keys_separated_by_slashes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for name in ["sub/deep/c.md", "a.md", "sub/b.pdf"] {
        touch(root, name);
    }

    let walked = walked(root);

    assert_eq!(keys(&walked), ["a.md", "sub/b.pdf", "sub/deep/c.md"]);
    assert_eq!(
        walked.files,
        [
            root.join("a.md"),
            root.join("sub").join("b.pdf"),
            root.join("sub").join("deep").join("c.md"),
        ]
    );
}

#[test]
fn the_file_type_comes_from_the_extension_and_the_time_from_the_file() {
    let directory = tempfile::tempdir().unwrap();
    touch(directory.path(), "a.pdf");
    let modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
    File::options()
        .write(true)
        .open(directory.path().join("a.pdf"))
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();

    let walked = walked(directory.path());

    let item = &walked.items.items[0];
    assert_eq!(item.file_type, FileType::for_test_double("pdf"));
    assert_eq!(item.time, modified);
}

#[test]
fn other_extensions_and_names_without_one_are_left_out_without_a_warning() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["a.txt", "README", "b.", "keep.md"] {
        touch(directory.path(), name);
    }

    let (walked, logs) = capture(|| walked(directory.path()));

    assert_eq!(keys(&walked), ["keep.md"]);
    assert!(walked.items.unreadable_without_key.is_empty());
    assert_eq!(walked.items.unreadable_places, 0);
    assert!(logs.is_empty());
}

#[test]
fn the_extension_is_given_to_format_as_it_is_written() {
    let directory = tempfile::tempdir().unwrap();
    touch(directory.path(), "A.MD");
    let format = TestFormat::new().with_extension("MD", "markdown");

    let walked = walk(directory.path(), "notes", &format).unwrap();

    assert_eq!(keys(&walked), ["A.MD"]);
}

#[test]
fn names_starting_with_a_dot_are_treated_like_any_other() {
    let directory = tempfile::tempdir().unwrap();
    for name in [".hidden/a.md", ".md", ".b.pdf"] {
        touch(directory.path(), name);
    }

    let walked = walked(directory.path());

    assert_eq!(keys(&walked), [".b.pdf", ".hidden/a.md", ".md"]);
}

#[test]
fn an_empty_directory_asks_to_delete_everything() {
    let directory = tempfile::tempdir().unwrap();

    let walked = walked(directory.path());

    assert!(walked.items.items.is_empty());
    assert_eq!(walked.items.delete_except_unreadable_dirs, Some(Vec::new()));
    assert_eq!(walked.items.unreadable_places, 0);
}

#[test]
fn a_walk_that_read_everything_asks_to_delete_with_no_unreadable_directories() {
    let directory = tempfile::tempdir().unwrap();
    touch(directory.path(), "sub/a.md");

    let walked = walked(directory.path());

    assert_eq!(walked.items.delete_except_unreadable_dirs, Some(Vec::new()));
}

#[cfg(unix)]
mod not_regular_files {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use super::*;

    #[test]
    fn a_link_inside_is_neither_followed_nor_a_document() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        touch(outside.path(), "linked.md");
        touch(directory.path(), "a.md");
        symlink(
            outside.path().join("linked.md"),
            directory.path().join("file-link.md"),
        )
        .unwrap();
        symlink(outside.path(), directory.path().join("dir-link")).unwrap();

        let walked = walked(directory.path());

        assert_eq!(keys(&walked), ["a.md"]);
    }

    #[test]
    fn a_link_at_the_location_is_followed() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        touch(&target, "a.md");
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();

        let walked = walked(&link);

        assert_eq!(keys(&walked), ["a.md"]);
    }

    #[test]
    fn a_link_at_the_location_is_judged_by_the_repository_it_leads_to() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        mark_git_repository(&repository);
        fs::write(repository.join(".gitignore"), "ignored.md\n").unwrap();
        touch(&repository, "ignored.md");
        touch(&repository, "kept.md");
        let link = directory.path().join("link");
        symlink(&repository, &link).unwrap();

        assert_eq!(keys(&walked(&link)), ["kept.md"]);
    }

    #[test]
    fn a_named_pipe_is_not_a_document() {
        let directory = tempfile::tempdir().unwrap();
        touch(directory.path(), "a.md");
        let pipe = directory.path().join("pipe.md");
        assert!(
            Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .unwrap()
                .success()
        );

        let walked = walked(directory.path());

        assert_eq!(keys(&walked), ["a.md"]);
    }
}

// `.gitignore`.

#[test]
fn in_a_git_repository_the_gitignore_rules_apply() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    mark_git_repository(root);
    fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
    touch(root, "ignored.md");
    touch(root, "kept.md");

    assert_eq!(keys(&walked(root)), ["kept.md"]);
}

#[test]
fn a_git_file_also_marks_a_repository() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
    touch(root, "ignored.md");
    touch(root, "kept.md");

    assert_eq!(keys(&walked(root)), ["kept.md"]);
}

#[test]
fn a_jj_directory_also_marks_a_repository() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join(".jj")).unwrap();
    fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
    touch(root, "ignored.md");
    touch(root, "kept.md");

    assert_eq!(keys(&walked(root)), ["kept.md"]);
}

#[test]
fn outside_a_repository_the_gitignore_rules_do_not_apply() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    if is_in_a_repository(root) {
        return;
    }
    fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
    touch(root, "ignored.md");

    assert_eq!(keys(&walked(root)), ["ignored.md"]);
}

#[test]
fn a_jj_file_does_not_mark_a_repository() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    if is_in_a_repository(root) {
        return;
    }
    fs::write(root.join(".jj"), "not a directory\n").unwrap();
    fs::write(root.join(".gitignore"), "kept.md\n").unwrap();
    touch(root, "kept.md");

    assert_eq!(keys(&walked(root)), ["kept.md"]);
}

#[test]
fn a_repository_below_the_directory_does_not_make_its_rules_apply() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    if is_in_a_repository(root) {
        return;
    }
    let nested = root.join("nested");
    mark_git_repository(&nested);
    fs::write(nested.join(".gitignore"), "kept.md\n").unwrap();
    touch(&nested, "kept.md");

    assert_eq!(keys(&walked(root)), ["nested/kept.md"]);
}

#[test]
fn a_repository_above_the_directory_is_found_from_its_parents() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path();
    mark_git_repository(repository);
    let root = repository.join("a").join("b");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".gitignore"), "ignored.md\n").unwrap();
    touch(&root, "ignored.md");
    touch(&root, "kept.md");

    assert_eq!(keys(&walked(&root)), ["kept.md"]);
}

#[test]
fn rules_above_the_directory_apply_to_what_is_inside_it() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path();
    mark_git_repository(repository);
    fs::write(repository.join(".gitignore"), "drafts/\n").unwrap();
    let root = repository.join("docs");
    touch(&root, "drafts/a.md");
    touch(&root, "kept.md");

    assert_eq!(keys(&walked(&root)), ["kept.md"]);
}

#[test]
fn rules_above_the_directory_do_not_apply_to_the_directory_itself() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path();
    mark_git_repository(repository);
    fs::write(repository.join(".gitignore"), "docs/\n").unwrap();
    let root = repository.join("docs");
    touch(&root, "a.md");

    assert_eq!(keys(&walked(&root)), ["a.md"]);
}

#[test]
fn the_exclude_file_and_ignore_files_are_not_read() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    mark_git_repository(root);
    fs::create_dir(root.join(".git").join("info")).unwrap();
    fs::write(root.join(".git").join("info").join("exclude"), "a.md\n").unwrap();
    fs::write(root.join(".ignore"), "b.md\n").unwrap();
    touch(root, "a.md");
    touch(root, "b.md");

    assert_eq!(keys(&walked(root)), ["a.md", "b.md"]);
}

#[test]
fn a_rule_that_cannot_be_parsed_is_not_an_unreadable_place() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    mark_git_repository(root);
    fs::write(root.join(".gitignore"), "a[\nignored.md\n").unwrap();
    touch(root, "kept.md");

    let (walked, logs) = capture(|| walked(root));

    assert_eq!(keys(&walked), ["kept.md"]);
    assert_eq!(walked.items.unreadable_places, 0);
    assert_eq!(walked.items.delete_except_unreadable_dirs, Some(Vec::new()));
    assert!(logs.is_empty());
}

// The directory itself cannot be read.

#[test]
fn a_missing_directory_is_an_error() {
    let directory = tempfile::tempdir().unwrap();

    let result = walk(&directory.path().join("gone"), "notes", &format());

    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
}

#[test]
fn a_file_at_the_location_is_an_error() {
    let directory = tempfile::tempdir().unwrap();
    touch(directory.path(), "a.md");

    let result = walk(&directory.path().join("a.md"), "notes", &format());

    assert!(result.is_err());
}

#[cfg(unix)]
mod unreadable {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Takes all permissions off `path` while it lives. `None` when the
    /// process can read it anyway, as the superuser can: the place cannot be
    /// made unreadable then, and the test has nothing to check.
    struct NoAccess<'a>(&'a Path);

    impl<'a> NoAccess<'a> {
        fn to(path: &'a Path) -> Option<Self> {
            fs::set_permissions(path, Permissions::from_mode(0o000)).unwrap();
            let guard = Self(path);
            fs::read_dir(path).is_err().then_some(guard)
        }
    }

    impl Drop for NoAccess<'_> {
        fn drop(&mut self) {
            fs::set_permissions(self.0, Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn a_directory_without_permission_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("locked");
        fs::create_dir(&root).unwrap();
        let Some(_access) = NoAccess::to(&root) else {
            return;
        };

        let result = walk(&root, "notes", &format());

        assert_eq!(
            result.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn a_subdirectory_that_cannot_be_read_is_warned_about_and_keeps_its_documents() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        touch(root, "a.md");
        touch(root, "sub/locked/b.md");
        let locked = root.join("sub").join("locked");
        let Some(_access) = NoAccess::to(&locked) else {
            return;
        };

        let (walked, logs) = capture(|| walked(root));

        assert_eq!(keys(&walked), ["a.md"]);
        assert_eq!(
            walked.items.delete_except_unreadable_dirs,
            Some(vec!["sub/locked".to_owned()])
        );
        assert_eq!(walked.items.unreadable_places, 1);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, Level::WARN);
        assert_eq!(field(&logs[0], "collection"), "notes");
        assert_eq!(field(&logs[0], "place"), "sub/locked");
        assert!(field(&logs[0], "cause").contains("ermission denied"));
    }

    #[test]
    fn two_unreadable_subdirectories_are_each_counted_and_listed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        touch(root, "one/a.md");
        touch(root, "two/b.md");
        let one = root.join("one");
        let two = root.join("two");
        let (Some(_one), Some(_two)) = (NoAccess::to(&one), NoAccess::to(&two)) else {
            return;
        };

        let walked = walked(root);

        assert_eq!(
            walked.items.delete_except_unreadable_dirs,
            Some(vec!["one".to_owned(), "two".to_owned()])
        );
        assert_eq!(walked.items.unreadable_places, 2);
    }
}

// Errors from the walker that a real directory cannot be made to give.

fn io_error() -> ignore::Error {
    ignore::Error::Io(io::Error::other("the disk went away"))
}

fn with_path(path: &Path) -> ignore::Error {
    ignore::Error::WithPath {
        path: path.to_path_buf(),
        err: Box::new(ignore::Error::WithDepth {
            depth: 1,
            err: Box::new(io_error()),
        }),
    }
}

fn after_error(root: &Path, error: ignore::Error) -> (IngestItems, Vec<LogRecord>) {
    let format = format();
    let mut state = Walk::new(root, "notes", &format);
    let ((), logs) = capture(|| state.take(Err(error)));
    (state.finish().items, logs)
}

#[test]
fn an_error_at_no_known_place_deletes_nothing_and_goes_on() {
    let directory = tempfile::tempdir().unwrap();
    let error = ignore::Error::WithDepth {
        depth: 2,
        err: Box::new(io_error()),
    };

    let (items, logs) = after_error(directory.path(), error);

    assert_eq!(items.delete_except_unreadable_dirs, None);
    assert_eq!(items.unreadable_places, 1);
    assert_eq!(logs.len(), 1);
    assert_eq!(field(&logs[0], "place"), "unknown");
    assert!(field(&logs[0], "cause").contains("the disk went away"));
}

#[test]
fn an_error_at_a_file_deletes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    touch(directory.path(), "a.md");
    let file = directory.path().join("a.md");

    let (items, logs) = after_error(directory.path(), with_path(&file));

    assert_eq!(items.delete_except_unreadable_dirs, None);
    assert_eq!(items.unreadable_places, 1);
    assert_eq!(field(&logs[0], "place"), "a.md");
}

#[test]
fn an_error_at_a_place_that_is_gone_deletes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let gone = directory.path().join("gone");

    let (items, _logs) = after_error(directory.path(), with_path(&gone));

    assert_eq!(items.delete_except_unreadable_dirs, None);
}

#[test]
fn an_error_at_the_directory_itself_deletes_nothing() {
    let directory = tempfile::tempdir().unwrap();

    let (items, logs) = after_error(directory.path(), with_path(directory.path()));

    assert_eq!(items.delete_except_unreadable_dirs, None);
    assert_eq!(field(&logs[0], "place"), ".");
}

#[test]
fn an_error_outside_the_directory_deletes_nothing_and_shows_the_whole_place() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let outside = directory.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();

    let (items, logs) = after_error(&root, with_path(&outside));

    assert_eq!(items.delete_except_unreadable_dirs, None);
    assert_eq!(field(&logs[0], "place"), outside.to_str().unwrap());
}

#[test]
fn an_error_at_a_directory_lists_it() {
    let directory = tempfile::tempdir().unwrap();
    let sub = directory.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let (items, _logs) = after_error(directory.path(), with_path(&sub));

    assert_eq!(
        items.delete_except_unreadable_dirs,
        Some(vec!["sub".to_owned()])
    );
}

#[test]
fn an_error_that_is_not_a_failure_to_read_is_not_counted() {
    let directory = tempfile::tempdir().unwrap();
    let glob = || ignore::Error::Glob {
        glob: Some("a[".to_owned()),
        err: "unclosed character class".to_owned(),
    };
    for error in [
        glob(),
        ignore::Error::Partial(vec![glob()]),
        ignore::Error::WithLineNumber {
            line: 1,
            err: Box::new(glob()),
        },
        ignore::Error::UnrecognizedFileType("md".to_owned()),
        ignore::Error::InvalidDefinition,
    ] {
        let (items, logs) = after_error(directory.path(), error);

        assert_eq!(items.unreadable_places, 0);
        assert_eq!(items.delete_except_unreadable_dirs, Some(Vec::new()));
        assert!(logs.is_empty());
    }
}

#[test]
fn a_loop_is_a_failure_to_read() {
    let directory = tempfile::tempdir().unwrap();
    let error = ignore::Error::Loop {
        ancestor: directory.path().to_path_buf(),
        child: directory.path().join("child"),
    };

    let (items, _logs) = after_error(directory.path(), error);

    assert_eq!(items.unreadable_places, 1);
    assert_eq!(items.delete_except_unreadable_dirs, None);
}

#[test]
fn an_error_with_a_line_number_still_tells_its_place() {
    let directory = tempfile::tempdir().unwrap();
    let sub = directory.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let error = ignore::Error::WithLineNumber {
        line: 3,
        err: Box::new(with_path(&sub)),
    };

    let (items, _logs) = after_error(directory.path(), error);

    assert_eq!(
        items.delete_except_unreadable_dirs,
        Some(vec!["sub".to_owned()])
    );
}

#[test]
fn a_read_failure_among_partial_errors_is_counted() {
    let directory = tempfile::tempdir().unwrap();
    let error = ignore::Error::Partial(vec![
        ignore::Error::Glob {
            glob: None,
            err: "bad".to_owned(),
        },
        io_error(),
    ]);

    let (items, _logs) = after_error(directory.path(), error);

    assert_eq!(items.unreadable_places, 1);
    assert_eq!(items.delete_except_unreadable_dirs, None);
}

#[test]
fn a_file_that_goes_away_before_its_time_is_read_deletes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    touch(root, "a.md");
    let format = format();
    let mut state = Walk::new(root, "notes", &format);
    let entry = walker(root)
        .map(Result::unwrap)
        .find(|entry| entry.file_name() == "a.md")
        .unwrap();
    fs::remove_file(root.join("a.md")).unwrap();

    let ((), logs) = capture(|| state.take(Ok(entry)));

    let walked = state.finish();
    assert!(walked.items.items.is_empty());
    assert!(walked.files.is_empty());
    assert_eq!(walked.items.delete_except_unreadable_dirs, None);
    assert_eq!(walked.items.unreadable_places, 1);
    assert_eq!(field(&logs[0], "place"), "a.md");
}

// Names that are not valid UTF-8. Linux can make such files; macOS refuses them
// with `Illegal byte sequence`, so the rules behind this are also tested on
// paths alone, in `names.rs`.

#[cfg(target_os = "linux")]
mod not_utf8 {
    use std::ffi::OsStr;
    use std::fs::Permissions;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_document_whose_name_is_not_utf8_has_no_key() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join(OsStr::from_bytes(b"a\xFFb.md")), b"x").unwrap();

        let walked = walked(root);

        assert!(walked.items.items.is_empty());
        assert!(walked.files.is_empty());
        let (name, _cause) = &walked.items.unreadable_without_key[0];
        assert_eq!(name, "a\u{FFFD}b.md");
    }

    #[test]
    fn a_file_whose_extension_is_not_utf8_is_not_a_document() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(OsStr::from_bytes(b"a.\xFF")), b"x").unwrap();

        let walked = walked(directory.path());

        assert!(walked.items.items.is_empty());
        assert!(walked.items.unreadable_without_key.is_empty());
    }

    #[test]
    fn an_unreadable_directory_whose_name_is_not_utf8_is_counted_but_not_listed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let locked = root.join(OsStr::from_bytes(b"\xFF"));
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let superuser_reads_anything = fs::read_dir(&locked).is_ok();

        let (walked, logs) = capture(|| walked(root));

        fs::set_permissions(&locked, Permissions::from_mode(0o700)).unwrap();
        if superuser_reads_anything {
            return;
        }
        assert_eq!(walked.items.delete_except_unreadable_dirs, Some(Vec::new()));
        assert_eq!(walked.items.unreadable_places, 1);
        assert_eq!(field(&logs[0], "place"), "\u{FFFD}");
    }
}
