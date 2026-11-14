//! What directory and archive collections take as documents, through updates
//! on the command line with the real components: which files and archive
//! items become documents, their keys, and their times. The details of the
//! archive formats, and the limits on what an archive may hold, are left to
//! the unit tests in `src/archive_ingest/`. So is a file in a directory whose
//! name is not UTF-8, to the unit tests in `src/file_ingest/`: the file
//! system of macOS, where these tests run, cannot hold such a name.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use crate::common::{Outcome, Workspace};

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// Writes `contents` to the file `name` in `dir`, creating the directories
/// on the way.
fn write(dir: &Path, name: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// Makes a directory collection named `name` on `dir`.
fn directory_collection(workspace: &Workspace, name: &str, dir: &Path) {
    assert_success(&workspace.run(&["collection", "add", name, &text(dir)]));
}

/// Makes an archive collection named `name` on a copy of the archive fixture
/// `fixture_name`, named `file_name`, and returns the copy.
fn archive_collection(
    workspace: &Workspace,
    name: &str,
    fixture_name: &str,
    file_name: &str,
) -> PathBuf {
    let archive = workspace.dir("archives").join(file_name);
    std::fs::copy(fixture(&format!("archives/{fixture_name}")), &archive).unwrap();
    add_archive(workspace, name, &archive);
    archive
}

fn add_archive(workspace: &Workspace, name: &str, archive: &Path) {
    assert_success(&workspace.run(&[
        "collection",
        "add",
        name,
        "--type",
        "archive",
        &text(archive),
    ]));
}

/// The text of the document `key` in `collection`, or `None` when there is no
/// such document.
fn got(workspace: &Workspace, collection: &str, key: &str) -> Option<String> {
    let got = workspace.run(&["get", collection, key]);
    if got.code == ExitCode::SUCCESS {
        return Some(got.stdout);
    }
    assert_eq!(got.code, ExitCode::from(1), "{got:?}");
    assert!(got.stderr.contains("does not exist"), "{got:?}");
    None
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// The summary of an update of the collection `name` with the six counts
/// `counts`, in the order the summary gives them.
fn updated(name: &str, counts: [u32; 6]) -> String {
    format!(
        "Updated the collection \"{name}\": {} added, {} changed, {} unchanged, {} re-ingested, {} deleted, {} failed.",
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
    )
}

/// The summary of an update of all `collections` with the six counts
/// `counts`.
fn updated_all(collections: u32, counts: [u32; 6]) -> String {
    format!(
        "Updated {collections} collections: {} added, {} changed, {} unchanged, {} re-ingested, {} deleted, {} failed.",
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
    )
}

/// Sets the last modification time of the file at `path` to 2001-09-09, long
/// before the test wrote the file.
fn set_an_old_modification_time(path: &Path) {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

#[cfg(unix)]
fn make_named_pipe(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a C string that lives until the call returns.
    let made = unsafe { libc::mkfifo(path.as_ptr(), 0o644) };
    assert_eq!(made, 0, "{}", std::io::Error::last_os_error());
}

/// A tar in the ustar form that holds `files`, each a name and a content.
/// The tests build it, as no archiver writes such names as `./docs//b.md`.
fn ustar(files: &[(&str, &str)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, content) in files {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        // The checksum counts its own field as eight spaces.
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        tar.extend_from_slice(&header);
        tar.extend_from_slice(content.as_bytes());
        tar.resize(tar.len().next_multiple_of(512), 0);
    }
    // The end-of-archive indicator: two blocks of zeros.
    tar.resize(tar.len() + 1024, 0);
    tar
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_directory_collection_takes_markdown_and_pdf_files_by_their_extension_in_any_case() {
    let workspace = Workspace::with_real_model();
    let notes = workspace.dir("notes");
    write(&notes, "a.md", "# A\n");
    write(&notes, "b.MARKDOWN", "# B\n");
    std::fs::copy(fixture("pdfs/english.pdf"), notes.join("c.Pdf")).unwrap();
    write(&notes, ".hidden.md", "# Hidden\n");
    write(&notes, ".dot/d.md", "# D\n");
    write(&notes, "e.txt", "E\n");
    write(&notes, "f", "F\n");
    directory_collection(&workspace, "notes", &notes);

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_success(&updated_notes);
    assert!(
        updated_notes
            .stderr
            .contains(&updated("notes", [5, 0, 0, 0, 0, 0])),
        "{updated_notes:?}"
    );
    for key in ["a.md", "b.MARKDOWN", "c.Pdf", ".hidden.md", ".dot/d.md"] {
        assert!(got(&workspace, "notes", key).is_some(), "{key}");
    }
    for key in ["e.txt", "f"] {
        assert_eq!(got(&workspace, "notes", key), None, "{key}");
    }
}

#[test]
#[cfg(unix)]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_directory_collection_leaves_out_links_and_named_pipes_and_follows_a_link_at_its_location() {
    let workspace = Workspace::with_real_model();
    let notes = workspace.dir("notes");
    write(&notes, "a.md", "# A\n");
    let elsewhere = workspace.dir("elsewhere");
    write(&elsewhere, "b.md", "# B\n");
    std::os::unix::fs::symlink(notes.join("a.md"), notes.join("link.md")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, notes.join("linked")).unwrap();
    make_named_pipe(&notes.join("pipe.md"));
    let location = workspace.dir("links").join("notes");
    std::os::unix::fs::symlink(&notes, &location).unwrap();
    directory_collection(&workspace, "notes", &location);

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_success(&updated_notes);
    assert!(
        updated_notes
            .stderr
            .contains(&updated("notes", [1, 0, 0, 0, 0, 0])),
        "{updated_notes:?}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
    for key in ["link.md", "linked/b.md", "pipe.md"] {
        assert_eq!(got(&workspace, "notes", key), None, "{key}");
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn in_a_git_repository_gitignore_rules_leave_files_out_but_not_the_directory_itself() {
    let workspace = Workspace::with_real_model();
    let repository = workspace.dir("repository");
    // `a.md` is taken if `notes/` in the `.gitignore` above leaves out what
    // is inside `notes` but not `notes` itself, and if neither the
    // repository's exclude file nor `.ignore` is read.
    write(&repository, ".git/info/exclude", "a.md\n");
    write(&repository, ".gitignore", "notes/\nsecret.md\n");
    let notes = repository.join("notes");
    write(&notes, "a.md", "# A\n");
    write(&notes, "secret.md", "# Secret\n");
    write(&notes, ".ignore", "a.md\n");
    directory_collection(&workspace, "notes", &notes);

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_success(&updated_notes);
    assert!(
        updated_notes
            .stderr
            .contains(&updated("notes", [1, 0, 0, 0, 0, 0])),
        "{updated_notes:?}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
    assert_eq!(got(&workspace, "notes", "secret.md"), None);
}

/// Makes a directory collection named `name` on a directory of the same name
/// that holds `a.md`, `secret.md`, and a `.gitignore` that leaves out
/// `secret.md`, and returns the directory.
fn with_a_gitignore(workspace: &Workspace, name: &str) -> PathBuf {
    let dir = workspace.dir(name);
    write(&dir, ".gitignore", "secret.md\n");
    write(&dir, "a.md", "# A\n");
    write(&dir, "secret.md", "# Secret\n");
    directory_collection(workspace, name, &dir);
    dir
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_jj_directory_or_a_git_file_marks_a_repository_and_outside_one_gitignore_is_not_read() {
    let workspace = Workspace::with_real_model();
    let jj = with_a_gitignore(&workspace, "jj");
    std::fs::create_dir(jj.join(".jj")).unwrap();
    let git = with_a_gitignore(&workspace, "git");
    write(&git, ".git", "gitdir: elsewhere\n");
    with_a_gitignore(&workspace, "plain");

    let updated_all_sources = workspace.run(&["update", "--all"]);

    assert_success(&updated_all_sources);
    assert!(
        updated_all_sources
            .stderr
            .contains(&updated_all(3, [4, 0, 0, 0, 0, 0])),
        "{updated_all_sources:?}"
    );
    assert_eq!(got(&workspace, "jj", "secret.md"), None);
    assert_eq!(got(&workspace, "git", "secret.md"), None);
    assert!(got(&workspace, "plain", "secret.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_directory_document_has_its_relative_path_as_its_key_and_a_new_file_time_makes_a_new_version() {
    let workspace = Workspace::with_real_model();
    let notes = workspace.dir("notes");
    write(&notes, "sub/dir/a.md", "# A\n");
    write(&notes, "b.md", "# B\n");
    directory_collection(&workspace, "notes", &notes);
    assert_success(&workspace.run(&["update", "notes"]));

    // Only the time of `a.md` changes, and with it the document's version.
    set_an_old_modification_time(&notes.join("sub/dir/a.md"));
    let updated_again = workspace.run(&["update", "notes"]);

    assert_success(&updated_again);
    assert!(
        updated_again
            .stderr
            .contains(&updated("notes", [0, 1, 1, 0, 0, 0])),
        "{updated_again:?}"
    );
    assert_eq!(
        got(&workspace, "notes", "sub/dir/a.md").as_deref(),
        Some("# A\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_archive_collection_reads_a_zip_a_tar_and_a_gzipped_tar_by_their_extension_in_any_case() {
    let workspace = Workspace::with_real_model();
    archive_collection(&workspace, "zip", "zip-command.zip", "a.ZIP");
    archive_collection(&workspace, "tgz", "gtar-ustar.tar.gz", "b.TGZ");
    archive_collection(&workspace, "targz", "gtar-ustar.tar.gz", "c.Tar.Gz");
    let tar = workspace.dir("archives").join("d.TAR");
    let mut unpacked = Vec::new();
    flate2::read::MultiGzDecoder::new(
        std::fs::File::open(fixture("archives/gtar-ustar.tar.gz")).unwrap(),
    )
    .read_to_end(&mut unpacked)
    .unwrap();
    std::fs::write(&tar, unpacked).unwrap();
    add_archive(&workspace, "tar", &tar);

    let updated_archives = workspace.run(&["update", "--all"]);

    assert_success(&updated_archives);
    // The zip holds four documents, and the tar three: its hard link and its
    // named pipe are not documents.
    assert!(
        updated_archives
            .stderr
            .contains(&updated_all(4, [13, 0, 0, 0, 0, 0])),
        "{updated_archives:?}"
    );
    assert!(got(&workspace, "zip", "notes/short.md").is_some());
    for collection in ["tgz", "targz", "tar"] {
        assert!(
            got(&workspace, collection, "日本語.md").is_some(),
            "{collection}"
        );
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_archive_collection_leaves_out_links_named_pipes_and_macos_metadata() {
    let workspace = Workspace::with_real_model();
    archive_collection(&workspace, "gnu", "gtar-gnu.tar.gz", "gnu.tar.gz");
    archive_collection(&workspace, "ditto", "ditto.zip", "ditto.zip");
    archive_collection(
        &workspace,
        "bsdtar",
        "bsdtar-mac-metadata.tar.gz",
        "bsdtar.tar.gz",
    );

    let updated_archives = workspace.run(&["update", "--all"]);

    assert_success(&updated_archives);
    assert!(
        updated_archives
            .stderr
            .contains(&updated_all(3, [8, 0, 0, 0, 0, 0])),
        "{updated_archives:?}"
    );
    // The hard link `notes/short.md` leads to `hard.md`, which is a document
    // of its own.
    assert!(got(&workspace, "gnu", "hard.md").is_some());
    for key in [
        "link-to-short.md",
        "long-link.md",
        "notes/short.md",
        "pipe.md",
    ] {
        assert_eq!(got(&workspace, "gnu", key), None, "{key}");
    }
    assert!(got(&workspace, "ditto", "zipsrc/tagged.md").is_some());
    assert_eq!(
        got(&workspace, "ditto", "__MACOSX/zipsrc/._tagged.md"),
        None
    );
    assert!(got(&workspace, "bsdtar", "tagged.md").is_some());
    assert_eq!(got(&workspace, "bsdtar", "._tagged.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn of_archive_items_with_the_same_key_the_last_is_taken() {
    let workspace = Workspace::with_real_model();
    // `a.md`, `./a.md`, and `a.md`, holding `# First`, `# Second`, and
    // `# Third`. The test below checks that `./` is dropped from a key.
    archive_collection(&workspace, "arch", "python-duplicate-names.zip", "a.zip");

    let updated_arch = workspace.run(&["update", "arch"]);

    assert_success(&updated_arch);
    assert_eq!(
        got(&workspace, "arch", "a.md").as_deref(),
        Some("# Third\n")
    );
    assert_eq!(got(&workspace, "arch", "./a.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_archive_item_key_turns_backslashes_into_slashes_and_drops_dot_and_empty_segments() {
    let workspace = Workspace::with_real_model();
    let archive = workspace.dir("archives").join("a.tar");
    std::fs::write(
        &archive,
        ustar(&[("./docs//b.md", "# B\n"), ("docs\\c.md", "# C\n")]),
    )
    .unwrap();
    add_archive(&workspace, "arch", &archive);

    let updated_arch = workspace.run(&["update", "arch"]);

    assert_success(&updated_arch);
    assert!(
        updated_arch
            .stderr
            .contains(&updated("arch", [2, 0, 0, 0, 0, 0])),
        "{updated_arch:?}"
    );
    assert_eq!(
        got(&workspace, "arch", "docs/b.md").as_deref(),
        Some("# B\n")
    );
    assert_eq!(
        got(&workspace, "arch", "docs/c.md").as_deref(),
        Some("# C\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_new_time_of_the_archive_makes_a_new_version_of_every_document() {
    let workspace = Workspace::with_real_model();
    let archive = archive_collection(&workspace, "arch", "zip-command.zip", "a.zip");
    assert_success(&workspace.run(&["update", "arch"]));

    set_an_old_modification_time(&archive);
    let updated_again = workspace.run(&["update", "arch"]);

    assert_success(&updated_again);
    assert!(
        updated_again
            .stderr
            .contains(&updated("arch", [0, 4, 0, 0, 0, 0])),
        "{updated_again:?}"
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_archive_item_whose_name_is_not_utf8_cannot_be_read_and_is_skipped_with_a_warning() {
    let workspace = Workspace::with_real_model();
    // Four documents, a symbolic link, and `latin1-\xe9.md`, whose name is
    // Latin-1.
    archive_collection(&workspace, "arch", "python-pax.tar.gz", "a.tar.gz");

    let output = workspace.run_executable(&["--log-level", "warn", "update", "arch"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&updated("arch", [4, 0, 0, 0, 0, 1])),
        "{stderr}"
    );
    // With no key, the warning gives the name with what is not UTF-8
    // replaced.
    assert!(
        stderr.lines().any(|line| {
            line.contains("cannot be read and was skipped")
                && line.contains("name=\"latin1-\u{fffd}.md\"")
                && line.contains("cause=")
        }),
        "{stderr}"
    );
    assert!(got(&workspace, "arch", "notes/short.md").is_some());
}
