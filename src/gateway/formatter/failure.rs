//! What mdya writes to stderr on an unexpected failure, besides the
//! explanation that asks for a bug report: where the panic happened, what it
//! said, and the backtrace, written when it happens, and what mdya was doing,
//! written where the failure is caught. Together, they let a bug report be
//! traced back to the code.

use super::replace_controls;

/// A panic, as the replaced panic hook sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PanicClue<'a> {
    /// mdya's version, such as `mdya 1.2.3`.
    pub(crate) version: &'a str,
    /// `file:line:column`, when the panic has a location.
    pub(crate) location: Option<&'a str>,
    /// The panic's message.
    pub(crate) message: &'a str,
    /// The backtrace as the standard library writes it, or `None` when none
    /// could be captured.
    pub(crate) backtrace: Option<&'a str>,
}

/// The panic as the replaced panic hook writes it: the version and the
/// location, the message, and the backtrace, each line ending with a line
/// break. Every line has its control characters and the characters that
/// change the direction of text replaced, and the line breaks between them
/// are kept.
pub(crate) fn human_panic(panic: &PanicClue<'_>) -> String {
    let first = match panic.location {
        Some(location) => format!("{} panicked at {location}:", panic.version),
        None => format!("{} panicked:", panic.version),
    };
    let backtrace = match panic.backtrace {
        Some(backtrace) => format!("Backtrace:\n{}", backtrace.trim_end_matches('\n')),
        None => "Backtrace: not available".to_owned(),
    };
    [first.as_str(), panic.message, backtrace.as_str()]
        .iter()
        .flat_map(|part| part.split('\n'))
        .map(|line| format!("{}\n", replace_controls(line)))
        .collect()
}

/// What mdya was doing when an unexpected failure happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailurePlace<'a> {
    /// Reading the command line arguments.
    ReadingArguments,
    /// Assembling the components for the command, such as `mdya update`.
    Preparing(&'a str),
    /// Running the command.
    Running(&'a str),
    /// A call of the MCP tool of this name.
    McpTool(&'a str),
}

/// The line that says what mdya was doing, with a line break, as in
/// `It happened while running mdya update.`.
pub(crate) fn human_failure_place(place: FailurePlace<'_>) -> String {
    let doing = match place {
        FailurePlace::ReadingArguments => "while reading the arguments".to_owned(),
        FailurePlace::Preparing(command) => format!("while preparing to run {command}"),
        FailurePlace::Running(command) => format!("while running {command}"),
        FailurePlace::McpTool(tool) => format!("in the MCP tool {tool}"),
    };
    format!("{}\n", replace_controls(&format!("It happened {doing}.")))
}

#[cfg(test)]
mod tests;
