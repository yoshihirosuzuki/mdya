//! Making a path absolute: a collection's location, the data directory, or
//! the embedding model cache directory. This module belongs to no
//! component. It is for `gateway`, to make the two directories absolute,
//! and for the ingest implementations whose type-specific attributes include a
//! location (`file_ingest` and `archive_ingest`), to make that location
//! absolute.

use std::ffi::OsString;
use std::io;
use std::path::{
    Component, Components, MAIN_SEPARATOR_STR, Path, PathBuf, Prefix, PrefixComponent,
};

use crate::types::DirLookup;

/// Makes a path absolute. A first component of `~` alone becomes the home
/// directory, and any other path that is not absolute is resolved from the
/// working directory. `.` components are removed and `..` components are
/// kept. The file system is not read, so symbolic links are not followed.
///
/// The home directory is looked up only when the first component is `~`
/// alone, and the working directory only when the path needs it: a relative
/// path, and on Windows, a path from the root (`\data`) or relative to a drive
/// (`C:docs`).
///
/// Fails when the home directory is needed but unknown, when the working
/// directory is needed but cannot be read, when a directory it looks up is not
/// absolute, or, on Windows, when the path is relative to a drive other than
/// the working directory's.
pub fn make_path_absolute(path: &Path, dirs: &DirLookup) -> Result<PathBuf, MakePathAbsoluteError> {
    if path.is_absolute() {
        return Ok(assemble(path.components()));
    }
    if let Some(rest) = under_home(path) {
        let home_dir = home_dir(dirs)?;
        return Ok(assemble(home_dir.components().chain(rest)));
    }
    let working_dir = working_dir(dirs)?;
    Ok(assemble(resolve_from_working_dir(path, &working_dir)?))
}

/// Why a path cannot be made absolute.
#[derive(Debug, thiserror::Error)]
pub enum MakePathAbsoluteError {
    #[error("the home directory is unknown, so a leading `~` cannot be expanded")]
    HomeDirUnknown,
    /// The home directory that was found, which is not absolute.
    #[error("the home directory is not an absolute path: {}", .0.display())]
    HomeDirNotAbsolute(PathBuf),
    #[error("the working directory cannot be read")]
    WorkingDirUnreadable(#[source] io::Error),
    /// The working directory that was found, which is not absolute.
    #[error("the working directory is not an absolute path: {}", .0.display())]
    WorkingDirNotAbsolute(PathBuf),
    #[error(
        "a drive-relative path on a drive other than the working directory's cannot be resolved"
    )]
    OtherDriveRelative,
}

fn home_dir(dirs: &DirLookup) -> Result<PathBuf, MakePathAbsoluteError> {
    let home_dir = (dirs.home_dir)().ok_or(MakePathAbsoluteError::HomeDirUnknown)?;
    if !home_dir.is_absolute() {
        return Err(MakePathAbsoluteError::HomeDirNotAbsolute(home_dir));
    }
    Ok(home_dir)
}

fn working_dir(dirs: &DirLookup) -> Result<PathBuf, MakePathAbsoluteError> {
    let working_dir = (dirs.current_dir)().map_err(MakePathAbsoluteError::WorkingDirUnreadable)?;
    if !working_dir.is_absolute() {
        return Err(MakePathAbsoluteError::WorkingDirNotAbsolute(working_dir));
    }
    Ok(working_dir)
}

/// The components after the first, when the first component is `~` alone.
/// They stay components rather than becoming a path again: parsed again, a
/// later component such as `C:docs` would read as a Windows drive.
fn under_home(path: &Path) -> Option<Components<'_>> {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => Some(components),
        _ => None,
    }
}

/// Resolves a path that is not absolute from the working directory, giving the
/// components of the absolute path before `.` components are removed. On
/// Windows, a path from the root (`\data`) takes the working directory's
/// prefix, and a drive-relative path (`C:docs`) is resolved from the working
/// directory only when both are on the same drive.
fn resolve_from_working_dir<'a>(
    path: &'a Path,
    working_dir: &'a Path,
) -> Result<Vec<Component<'a>>, MakePathAbsoluteError> {
    let mut components = path.components();
    match components.next() {
        Some(Component::RootDir) => Ok(prefix_of(working_dir).chain(path.components()).collect()),
        Some(Component::Prefix(drive)) if same_drive(drive, working_dir) => {
            Ok(working_dir.components().chain(components).collect())
        }
        Some(Component::Prefix(_)) => Err(MakePathAbsoluteError::OtherDriveRelative),
        _ => Ok(working_dir.components().chain(path.components()).collect()),
    }
}

fn prefix_of(path: &Path) -> impl Iterator<Item = Component<'_>> {
    path.components()
        .take_while(|c| matches!(c, Component::Prefix(_)))
}

fn same_drive(prefix: PrefixComponent<'_>, working_dir: &Path) -> bool {
    let Some(Component::Prefix(working)) = working_dir.components().next() else {
        return false;
    };
    drive_letter(prefix).is_some_and(|letter| drive_letter(working) == Some(letter))
}

fn drive_letter(prefix: PrefixComponent<'_>) -> Option<u8> {
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter.to_ascii_uppercase()),
        _ => None,
    }
}

/// Builds the path from its components, without `.` components. Built by hand
/// rather than with `Path::join` or by collecting into a `PathBuf`, because
/// joining or pushing onto a verbatim Windows path (`\\?\C:\...`) removes
/// `..`.
fn assemble<'a>(components: impl IntoIterator<Item = Component<'a>>) -> PathBuf {
    let mut path = OsString::new();
    let mut after_name = false;
    for component in components {
        match component {
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => {
                path.push(component.as_os_str());
                after_name = false;
            }
            Component::ParentDir | Component::Normal(_) => {
                if after_name {
                    path.push(MAIN_SEPARATOR_STR);
                }
                path.push(component.as_os_str());
                after_name = true;
            }
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::types::test_double::test_dir_lookup;

    /// A `DirLookup` for checking that a directory is not looked up: each
    /// function chosen here fails the test when called, and each other one
    /// answers as `test_dir_lookup(Some("/home/me"), Ok("/work"))` does.
    fn dirs_not_to_look_up(panic_on_home_dir: bool, panic_on_current_dir: bool) -> DirLookup {
        let answers = test_dir_lookup(Some("/home/me"), Ok("/work"));
        DirLookup {
            home_dir: if panic_on_home_dir {
                Arc::new(|| panic!("the home directory was looked up"))
            } else {
                answers.home_dir
            },
            current_dir: if panic_on_current_dir {
                Arc::new(|| panic!("the working directory was looked up"))
            } else {
                answers.current_dir
            },
        }
    }

    #[test]
    fn the_message_of_a_directory_that_is_not_absolute_tells_the_directory() {
        let home_dir = MakePathAbsoluteError::HomeDirNotAbsolute(PathBuf::from("me"));
        let working_dir = MakePathAbsoluteError::WorkingDirNotAbsolute(PathBuf::from("work"));

        assert_eq!(
            home_dir.to_string(),
            "the home directory is not an absolute path: me"
        );
        assert_eq!(
            working_dir.to_string(),
            "the working directory is not an absolute path: work"
        );
    }

    // Paths are compared as strings: `Path` equality skips `.` components,
    // so it cannot tell whether they were removed.
    #[cfg(unix)]
    mod unix {
        use super::*;

        fn resolve(location: &str) -> String {
            let dirs = test_dir_lookup(Some("/home/me"), Ok("/work"));
            let location = make_path_absolute(Path::new(location), &dirs).unwrap();
            location.to_str().unwrap().to_owned()
        }

        #[test]
        fn docs_slash_notes_is_resolved_from_the_working_directory() {
            assert_eq!(resolve("docs/notes"), "/work/docs/notes");
        }

        #[test]
        fn an_absolute_location_stays_as_it_is() {
            assert_eq!(resolve("/data/docs"), "/data/docs");
        }

        #[test]
        fn an_empty_location_is_the_working_directory() {
            assert_eq!(resolve(""), "/work");
        }

        #[test]
        fn a_lone_tilde_is_the_home_directory() {
            assert_eq!(resolve("~"), "/home/me");
        }

        #[test]
        fn tilde_slash_notes_is_under_the_home_directory() {
            assert_eq!(resolve("~/notes"), "/home/me/notes");
        }

        #[test]
        fn tilde_followed_by_a_name_is_a_relative_location() {
            assert_eq!(resolve("~user/notes"), "/work/~user/notes");
        }

        #[test]
        fn a_tilde_after_the_first_component_is_a_name() {
            assert_eq!(resolve("notes/~"), "/work/notes/~");
        }

        #[test]
        fn dot_components_are_removed() {
            assert_eq!(resolve("./a/./b/."), "/work/a/b");
            assert_eq!(resolve("/data/./x/."), "/data/x");
            assert_eq!(resolve("~/./x"), "/home/me/x");
        }

        #[test]
        fn dot_dot_components_are_kept() {
            assert_eq!(resolve("../a"), "/work/../a");
            assert_eq!(resolve("/data/../x"), "/data/../x");
            assert_eq!(resolve("~/../x"), "/home/me/../x");
        }

        #[test]
        fn control_characters_in_a_location_are_kept_as_they_are() {
            assert_eq!(resolve("a\u{7}b/\u{1b}c"), "/work/a\u{7}b/\u{1b}c");
        }

        #[test]
        fn a_trailing_slash_is_dropped() {
            assert_eq!(resolve("docs/"), "/work/docs");
        }

        #[test]
        fn a_symbolic_link_on_the_way_is_not_followed() {
            let directory = tempfile::tempdir().unwrap();
            std::fs::create_dir(directory.path().join("target")).unwrap();
            std::os::unix::fs::symlink("target", directory.path().join("link")).unwrap();
            let working_dir = directory.path().to_str().unwrap();
            let dirs = test_dir_lookup(None, Ok(working_dir));

            let location = make_path_absolute(Path::new("link/x"), &dirs).unwrap();

            assert_eq!(
                location.as_os_str(),
                directory.path().join("link").join("x").as_os_str()
            );
        }

        #[test]
        fn a_tilde_without_a_home_directory_fails_because_the_home_directory_is_unknown() {
            let dirs = test_dir_lookup(None, Ok("/work"));

            let result = make_path_absolute(Path::new("~/notes"), &dirs);

            assert!(matches!(result, Err(MakePathAbsoluteError::HomeDirUnknown)));
        }

        #[test]
        fn a_relative_location_does_not_need_the_home_directory() {
            let dirs = test_dir_lookup(None, Ok("/work"));

            let location = make_path_absolute(Path::new("docs"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), "/work/docs");
        }

        #[test]
        fn a_tilde_with_a_relative_home_directory_fails_and_tells_the_home_directory() {
            let dirs = test_dir_lookup(Some("me"), Ok("/work"));

            let result = make_path_absolute(Path::new("~/notes"), &dirs);

            let Err(MakePathAbsoluteError::HomeDirNotAbsolute(home_dir)) = result else {
                panic!("expected HomeDirNotAbsolute, got {result:?}");
            };
            assert_eq!(home_dir, PathBuf::from("me"));
        }

        #[test]
        fn a_relative_location_with_an_unreadable_working_directory_fails_with_the_cause() {
            let dirs = test_dir_lookup(Some("/home/me"), Err(io::ErrorKind::NotFound));

            let result = make_path_absolute(Path::new("docs"), &dirs);

            let Err(MakePathAbsoluteError::WorkingDirUnreadable(cause)) = result else {
                panic!("expected WorkingDirUnreadable, got {result:?}");
            };
            assert_eq!(cause.kind(), io::ErrorKind::NotFound);
        }

        #[test]
        fn a_relative_location_with_a_relative_working_directory_fails_and_tells_it() {
            let dirs = test_dir_lookup(Some("/home/me"), Ok("work"));

            let result = make_path_absolute(Path::new("docs"), &dirs);

            let Err(MakePathAbsoluteError::WorkingDirNotAbsolute(working_dir)) = result else {
                panic!("expected WorkingDirNotAbsolute, got {result:?}");
            };
            assert_eq!(working_dir, PathBuf::from("work"));
        }

        #[test]
        fn an_absolute_location_looks_up_neither_directory() {
            let dirs = dirs_not_to_look_up(true, true);

            let location = make_path_absolute(Path::new("/data/docs"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), "/data/docs");
        }

        #[test]
        fn a_location_under_the_home_directory_does_not_look_up_the_working_directory() {
            let dirs = dirs_not_to_look_up(false, true);

            let location = make_path_absolute(Path::new("~/notes"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), "/home/me/notes");
        }

        #[test]
        fn a_relative_location_does_not_look_up_the_home_directory() {
            let dirs = dirs_not_to_look_up(true, false);

            let location = make_path_absolute(Path::new("docs"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), "/work/docs");
        }

        #[test]
        fn tilde_followed_by_a_name_does_not_look_up_the_home_directory() {
            let dirs = dirs_not_to_look_up(true, false);

            let location = make_path_absolute(Path::new("~user/notes"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), "/work/~user/notes");
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn absolute(location: &str) -> Result<PathBuf, MakePathAbsoluteError> {
            let dirs = test_dir_lookup(Some(r"C:\Users\me"), Ok(r"D:\work"));
            make_path_absolute(Path::new(location), &dirs)
        }

        fn resolve(location: &str) -> String {
            absolute(location).unwrap().to_str().unwrap().to_owned()
        }

        fn resolve_from(location: &str, working_dir: &str, home_dir: &str) -> String {
            let dirs = test_dir_lookup(Some(home_dir), Ok(working_dir));
            let location = make_path_absolute(Path::new(location), &dirs).unwrap();
            location.to_str().unwrap().to_owned()
        }

        #[test]
        fn tilde_backslash_notes_is_under_the_home_directory() {
            assert_eq!(resolve(r"~\notes"), r"C:\Users\me\notes");
        }

        #[test]
        fn a_location_from_the_root_is_on_the_drive_of_the_working_directory() {
            assert_eq!(resolve(r"\data"), r"D:\data");
        }

        #[test]
        fn a_drive_relative_location_on_the_drive_of_the_working_directory_is_resolved_from_it() {
            assert_eq!(resolve("d:docs"), r"D:\work\docs");
        }

        #[test]
        fn a_drive_relative_location_on_another_drive_fails() {
            assert!(matches!(
                absolute("C:docs"),
                Err(MakePathAbsoluteError::OtherDriveRelative)
            ));
        }

        #[test]
        fn dot_components_are_removed_from_an_absolute_location() {
            assert_eq!(resolve(r"C:\data\.\x"), r"C:\data\x");
        }

        #[test]
        fn a_name_like_a_drive_under_the_home_directory_stays_a_name() {
            assert_eq!(resolve("~/C:docs"), r"C:\Users\me\C:docs");
        }

        // `PathBuf::push` removes `.` and `..` when the path it pushes onto
        // has a verbatim prefix; these check that `..` is still kept.
        #[test]
        fn a_verbatim_absolute_location_keeps_dot_dot_and_loses_dot() {
            assert_eq!(resolve(r"\\?\C:\a\.\b\..\c"), r"\\?\C:\a\b\..\c");
        }

        #[test]
        fn a_relative_location_under_a_verbatim_working_directory_keeps_dot_dot() {
            let working_dir = r"\\?\D:\work";
            let home_dir = r"C:\Users\me";

            assert_eq!(
                resolve_from(r"..\x", working_dir, home_dir),
                r"\\?\D:\work\..\x"
            );
            assert_eq!(
                resolve_from(r".\y", working_dir, home_dir),
                r"\\?\D:\work\y"
            );
            assert_eq!(
                resolve_from("d:docs", working_dir, home_dir),
                r"\\?\D:\work\docs"
            );
        }

        #[test]
        fn a_location_under_a_verbatim_home_directory_keeps_dot_dot() {
            assert_eq!(
                resolve_from(r"~\..\x", r"D:\work", r"\\?\C:\Users\me"),
                r"\\?\C:\Users\me\..\x"
            );
        }

        #[test]
        fn a_unc_absolute_location_loses_dot() {
            assert_eq!(resolve(r"\\server\share\a\.\b"), r"\\server\share\a\b");
        }

        #[test]
        fn a_location_from_the_root_is_on_the_share_of_a_unc_working_directory() {
            assert_eq!(
                resolve_from(r"\data", r"\\server\share\work", r"C:\Users\me"),
                r"\\server\share\data"
            );
        }

        #[test]
        fn an_absolute_location_with_a_drive_looks_up_neither_directory() {
            let dirs = dirs_not_to_look_up(true, true);

            let location = make_path_absolute(Path::new(r"C:\data"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), r"C:\data");
        }

        #[test]
        fn a_location_from_the_root_does_not_look_up_the_home_directory() {
            let dirs = DirLookup {
                current_dir: test_dir_lookup(None, Ok(r"D:\work")).current_dir,
                ..dirs_not_to_look_up(true, false)
            };

            let location = make_path_absolute(Path::new(r"\data"), &dirs).unwrap();

            assert_eq!(location.as_os_str(), r"D:\data");
        }

        #[test]
        fn a_home_directory_without_a_drive_is_not_absolute() {
            let dirs = test_dir_lookup(Some(r"\Users\me"), Ok(r"D:\work"));

            let result = make_path_absolute(Path::new(r"~\notes"), &dirs);

            assert!(matches!(
                result,
                Err(MakePathAbsoluteError::HomeDirNotAbsolute(_))
            ));
        }
    }
}
