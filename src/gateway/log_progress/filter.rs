//! Checks the directives of `--log-level` and `RUST_LOG`.

use std::ffi::OsStr;
use std::str::FromStr;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{Directive, LevelFilter};

use super::{IgnoredDirective, InvalidLogLevel, LogFilter};

/// The directive used without `--log-level` and without a valid directive in
/// `RUST_LOG`.
const DEFAULT_DIRECTIVE: &str = "warn";

pub(super) fn log_filter(
    flag: Option<&str>,
    env: Option<&OsStr>,
) -> Result<(LogFilter, Vec<IgnoredDirective>), InvalidLogLevel> {
    match flag {
        Some(flag) => Ok((filter_of_log_level(flag)?, Vec::new())),
        None => Ok(filter_of_rust_log(env.unwrap_or_default())),
    }
}

fn filter_of_log_level(flag: &str) -> Result<LogFilter, InvalidLogLevel> {
    let invalid = |detail| InvalidLogLevel {
        value: flag.to_owned(),
        detail,
    };
    let directives: Vec<&str> = flag
        .split(',')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .collect();
    if directives.is_empty() {
        return Err(invalid("it has no directives".to_owned()));
    }
    for directive in &directives {
        check(directive).map_err(|error| invalid(format!("`{directive}`: {error}")))?;
    }
    Ok(LogFilter::of(&directives))
}

fn filter_of_rust_log(env: &OsStr) -> (LogFilter, Vec<IgnoredDirective>) {
    let mut valid = Vec::new();
    let mut ignored = Vec::new();
    // Split the bytes, not a lossy string, so that a part that is not UTF-8
    // spoils only the directive it is in.
    for piece in env.as_encoded_bytes().split(|&byte| byte == b',') {
        match env_directive(piece) {
            Ok(Some(directive)) => valid.push(directive),
            Ok(None) => {}
            Err(directive) => ignored.push(directive),
        }
    }
    (LogFilter::of(&valid), ignored)
}

/// Reads one directive of `RUST_LOG`: `None` if it is empty.
fn env_directive(piece: &[u8]) -> Result<Option<&str>, IgnoredDirective> {
    let Ok(text) = std::str::from_utf8(piece) else {
        return Err(IgnoredDirective {
            directive: String::from_utf8_lossy(piece).trim().to_owned(),
            detail: "it is not valid UTF-8".to_owned(),
        });
    };
    let directive = text.trim();
    if directive.is_empty() {
        return Ok(None);
    }
    match check(directive) {
        Ok(()) => Ok(Some(directive)),
        Err(detail) => Err(IgnoredDirective {
            directive: directive.to_owned(),
            detail,
        }),
    }
}

/// Checks one directive, without the spaces around it: `EnvFilter`'s parser
/// takes ` warn` for the name of a target and rejects ` mdya=debug`. Returns
/// what is wrong with it.
fn check(directive: &str) -> Result<(), String> {
    // `EnvFilter` takes a word that is not a level for a target, and enables
    // only that target.
    if !directive.contains(['=', '[']) {
        return LevelFilter::from_str(directive)
            .map(drop)
            .map_err(|error| error.to_string());
    }
    // `EnvFilter` keeps the spaces in the name of a target, which then
    // matches no target.
    let target = directive.split(['=', '[']).next().unwrap_or_default();
    if target.contains(char::is_whitespace) {
        return Err("the name of a target has no spaces".to_owned());
    }
    Directive::from_str(directive)
        .map(drop)
        .map_err(|error| error.to_string())
}

impl LogFilter {
    /// The filter of the given valid directives, or of the default one if
    /// there are none.
    fn of(directives: &[&str]) -> Self {
        let directives = match directives {
            [] => DEFAULT_DIRECTIVE.to_owned(),
            directives => directives.join(","),
        };
        Self { directives }
    }

    /// The `EnvFilter` of the directives.
    #[expect(
        clippy::expect_used,
        reason = "every directive was parsed by itself when it was checked"
    )]
    pub(super) fn env_filter(&self) -> EnvFilter {
        EnvFilter::builder()
            .parse(&self.directives)
            .expect("`log_filter` checked every directive, and none has a comma in it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(directives: &str) -> LogFilter {
        LogFilter {
            directives: directives.to_owned(),
        }
    }

    fn ignored_directives(ignored: &[IgnoredDirective]) -> Vec<&str> {
        ignored
            .iter()
            .map(|ignored| ignored.directive.as_str())
            .collect()
    }

    #[test]
    fn without_the_flag_and_rust_log_the_logs_from_warn_up_are_written() {
        assert_eq!(log_filter(None, None).unwrap(), (filter("warn"), vec![]));
    }

    #[test]
    fn an_empty_rust_log_is_the_same_as_none() {
        assert_eq!(
            log_filter(None, Some(OsStr::new(""))).unwrap(),
            (filter("warn"), vec![])
        );
    }

    #[test]
    fn the_flag_is_used() {
        assert_eq!(
            log_filter(Some("debug"), None).unwrap(),
            (filter("debug"), vec![])
        );
    }

    #[test]
    fn with_the_flag_rust_log_is_not_read() {
        assert_eq!(
            log_filter(Some("info"), Some(OsStr::new("debug,wran"))).unwrap(),
            (filter("info"), vec![])
        );
    }

    #[test]
    fn directives_for_targets_and_spans_are_accepted() {
        let directives = "warn,mdya=debug,lance::io=trace,[update]=info";

        assert_eq!(
            log_filter(Some(directives), None).unwrap(),
            (filter(directives), vec![])
        );
    }

    #[test]
    fn levels_in_capitals_and_off_are_accepted() {
        assert_eq!(
            log_filter(Some("WARN,lance=off"), None).unwrap(),
            (filter("WARN,lance=off"), vec![])
        );
    }

    #[test]
    fn the_spaces_around_each_directive_are_removed() {
        assert_eq!(
            log_filter(Some(" warn , mdya=debug "), None).unwrap(),
            (filter("warn,mdya=debug"), vec![])
        );
    }

    #[test]
    fn empty_directives_are_skipped() {
        assert_eq!(
            log_filter(Some("warn,,mdya=debug,"), None).unwrap(),
            (filter("warn,mdya=debug"), vec![])
        );
    }

    #[test]
    fn a_flag_without_a_directive_is_an_error() {
        for value in ["", " ", ",,"] {
            let error = log_filter(Some(value), None).unwrap_err();

            assert_eq!(error.value, value);
        }
    }

    #[test]
    fn a_misspelt_level_in_the_flag_is_an_error_that_names_it() {
        let error = log_filter(Some("warn,wran"), None).unwrap_err();

        assert_eq!(error.value, "warn,wran");
        assert!(error.detail.starts_with("`wran`: "), "{}", error.detail);
    }

    #[test]
    fn a_target_without_a_level_in_the_flag_is_an_error() {
        let error = log_filter(Some("mdya"), None).unwrap_err();

        assert!(error.detail.starts_with("`mdya`: "), "{}", error.detail);
    }

    #[test]
    fn a_misspelt_level_after_a_target_in_the_flag_is_an_error() {
        let error = log_filter(Some("mdya=verbose"), None).unwrap_err();

        assert!(
            error.detail.starts_with("`mdya=verbose`: "),
            "{}",
            error.detail
        );
    }

    #[test]
    fn a_target_with_a_space_in_it_is_an_error() {
        // `EnvFilter` would take `mdya ` for the name of a target, which no
        // target has.
        let error = log_filter(Some("mdya =debug"), None).unwrap_err();

        assert!(
            error.detail.starts_with("`mdya =debug`: "),
            "{}",
            error.detail
        );
    }

    #[test]
    fn a_word_with_a_space_in_it_is_an_error_about_the_level() {
        let error = log_filter(Some("warn debug"), None).unwrap_err();

        assert!(error.detail.contains("expected one of"), "{}", error.detail);
    }

    #[test]
    fn a_space_in_a_span_filter_is_left_to_the_parser() {
        let directives = "[update{collection=my notes}]=debug";

        assert_eq!(
            log_filter(Some(directives), None).unwrap(),
            (filter(directives), vec![])
        );
    }

    #[test]
    fn invalid_directives_in_rust_log_are_ignored_and_the_rest_are_used() {
        let (filter_used, ignored) =
            log_filter(None, Some(OsStr::new("mdya=debug, wran ,info"))).unwrap();

        assert_eq!(filter_used, filter("mdya=debug,info"));
        assert_eq!(ignored_directives(&ignored), ["wran"]);
    }

    #[test]
    fn without_a_valid_directive_in_rust_log_the_logs_from_warn_up_are_written() {
        let (filter_used, ignored) =
            log_filter(None, Some(OsStr::new("wran,mdya,mdya=verbose"))).unwrap();

        assert_eq!(filter_used, filter("warn"));
        assert_eq!(
            ignored_directives(&ignored),
            ["wran", "mdya", "mdya=verbose"]
        );
    }

    #[test]
    fn an_ignored_directive_says_what_is_wrong_with_it() {
        let (_, ignored) = log_filter(None, Some(OsStr::new("wran"))).unwrap();

        assert!(
            ignored[0].detail.contains("expected one of"),
            "{}",
            ignored[0].detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directive_in_rust_log_that_is_not_utf_8_is_ignored() {
        use std::os::unix::ffi::OsStrExt;
        let env = OsStr::from_bytes(b"mdya=debug,lance=\xff");

        let (filter_used, ignored) = log_filter(None, Some(env)).unwrap();

        assert_eq!(filter_used, filter("mdya=debug"));
        assert_eq!(
            ignored,
            [IgnoredDirective {
                directive: "lance=\u{fffd}".to_owned(),
                detail: "it is not valid UTF-8".to_owned(),
            }]
        );
    }

    #[test]
    fn no_level_is_turned_off_when_built() {
        // `EnvFilter` writes a warning straight to stderr, over the one-line
        // progress display, for a directive finer than this level. This sees
        // the `max_level_*` features of `tracing`; the `release_max_level_*`
        // ones take effect only without debug assertions, so `cargo test`
        // without `--release`, as in `just check`, does not see them.
        assert_eq!(
            tracing::level_filters::STATIC_MAX_LEVEL,
            tracing::level_filters::LevelFilter::TRACE
        );
    }
}
