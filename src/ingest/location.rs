//! Making a location absolute, for the ingest implementations whose
//! type-specific attributes include a location (`file_ingest` and
//! `archive_ingest`).

use std::ffi::OsString;
use std::path::{
    Component, Components, MAIN_SEPARATOR_STR, Path, PathBuf, Prefix, PrefixComponent,
};

use super::{IngestError, LocationBase};

/// Makes a location absolute. A first component of `~` alone becomes the home
/// directory, and any other location that is not absolute is resolved from
/// the working directory. `.` components are removed and `..` components are
/// kept. The file system is not read, so symbolic links are not followed.
///
/// Returns [`IngestError::LocationNotFound`] when the first component is `~`
/// alone and the home directory is unknown, or, on Windows, when the location
/// is relative to a drive other than the working directory's. The directories
/// in `base` are expected to be absolute.
pub fn absolute_location(location: &str, base: &LocationBase) -> Result<PathBuf, IngestError> {
    let components =
        resolve(Path::new(location), base).map_err(|cause| IngestError::LocationNotFound {
            source: Box::new(cause),
        })?;
    Ok(assemble(components))
}

/// Why a location cannot be made absolute: the `source` of
/// [`IngestError::LocationNotFound`].
#[derive(Debug, thiserror::Error)]
enum Unresolvable {
    #[error("the home directory is unknown, so a leading `~` cannot be expanded")]
    HomeDirUnknown,
    #[error(
        "a drive-relative location on a drive other than the working directory's cannot be resolved"
    )]
    OtherDriveRelative,
}

/// The components of the absolute location, before `.` components are
/// removed. Built from components rather than with `Path::join`, because
/// joining onto a verbatim Windows path (`\\?\C:\...`) removes `..`.
fn resolve<'a>(
    location: &'a Path,
    base: &'a LocationBase,
) -> Result<Vec<Component<'a>>, Unresolvable> {
    if location.is_absolute() {
        return Ok(location.components().collect());
    }
    if let Some(rest) = under_home(location) {
        let home = base
            .home_dir
            .as_deref()
            .ok_or(Unresolvable::HomeDirUnknown)?;
        return Ok(home.components().chain(rest).collect());
    }
    resolve_from_working_dir(location, &base.working_dir)
}

/// The components after the first, when the first component is `~` alone.
/// They stay components rather than becoming a path again: parsed again, a
/// later component such as `C:docs` would read as a Windows drive.
fn under_home(location: &Path) -> Option<Components<'_>> {
    let mut components = location.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "~" => Some(components),
        _ => None,
    }
}

/// Resolves a location that is not absolute from the working directory. On
/// Windows, a location from the root (`\data`) takes the working directory's
/// prefix, and a drive-relative location (`C:docs`) is resolved from the
/// working directory only when both are on the same drive.
fn resolve_from_working_dir<'a>(
    location: &'a Path,
    working_dir: &'a Path,
) -> Result<Vec<Component<'a>>, Unresolvable> {
    let mut components = location.components();
    match components.next() {
        Some(Component::RootDir) => Ok(prefix_of(working_dir)
            .chain(location.components())
            .collect()),
        Some(Component::Prefix(drive)) if same_drive(drive, working_dir) => {
            Ok(working_dir.components().chain(components).collect())
        }
        Some(Component::Prefix(_)) => Err(Unresolvable::OtherDriveRelative),
        _ => Ok(working_dir
            .components()
            .chain(location.components())
            .collect()),
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
/// rather than collected into a `PathBuf`, because pushing onto a verbatim
/// Windows path removes `..`.
fn assemble(components: Vec<Component<'_>>) -> PathBuf {
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
    use super::*;

    fn base(working_dir: &str, home_dir: Option<&str>) -> LocationBase {
        LocationBase {
            working_dir: PathBuf::from(working_dir),
            home_dir: home_dir.map(PathBuf::from),
        }
    }

    /// The cause inside the `LocationNotFound` that `result` should be.
    fn not_found_cause(result: Result<PathBuf, IngestError>) -> Unresolvable {
        let Err(IngestError::LocationNotFound { source }) = result else {
            panic!("expected LocationNotFound, got {result:?}");
        };
        *source.downcast::<Unresolvable>().unwrap()
    }

    // Paths are compared as strings: `Path` equality skips `.` components,
    // so it cannot tell whether they were removed.
    #[cfg(unix)]
    mod unix {
        use super::*;

        fn resolve(location: &str) -> String {
            let base = base("/work", Some("/home/me"));
            let location = absolute_location(location, &base).unwrap();
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

            let location = absolute_location("link/x", &base(working_dir, None)).unwrap();

            assert_eq!(
                location.as_os_str(),
                directory.path().join("link").join("x").as_os_str()
            );
        }

        #[test]
        fn a_tilde_without_a_home_directory_is_not_found_because_the_home_directory_is_unknown() {
            let result = absolute_location("~/notes", &base("/work", None));

            assert!(matches!(
                not_found_cause(result),
                Unresolvable::HomeDirUnknown
            ));
        }

        #[test]
        fn a_relative_location_does_not_need_the_home_directory() {
            let location = absolute_location("docs", &base("/work", None)).unwrap();

            assert_eq!(location.as_os_str(), "/work/docs");
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn absolute(location: &str) -> Result<PathBuf, IngestError> {
            absolute_location(location, &base(r"D:\work", Some(r"C:\Users\me")))
        }

        fn resolve(location: &str) -> String {
            absolute(location).unwrap().to_str().unwrap().to_owned()
        }

        fn resolve_from(location: &str, working_dir: &str, home_dir: &str) -> String {
            let base = base(working_dir, Some(home_dir));
            let location = absolute_location(location, &base).unwrap();
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
        fn a_drive_relative_location_on_another_drive_is_not_found() {
            assert!(matches!(
                not_found_cause(absolute("C:docs")),
                Unresolvable::OtherDriveRelative
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
    }
}
