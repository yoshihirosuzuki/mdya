//! Makes what mdya shows the user, for each output format, from what the
//! command line, the MCP server, and `entry` decide to show, so that it
//! keeps to the rules on characters and numbers whatever it is given: the
//! human-readable output has its control characters and the characters that
//! change the direction of text replaced with visible forms and holds no NaN
//! or infinity; JSON holds no NaN, no infinity, and no integer outside
//! −(2^53 − 1) to 2^53 − 1; the command line's JSON also escapes the
//! characters that change the direction of text; and what is passed to the
//! MCP library keeps its strings as they are, as the MCP library escapes
//! what JSON requires. The callers write or pass on what it makes without
//! changing it. It writes nothing itself. The logs do not go through it:
//! `log_progress` writes them, and uses the rules on characters from here.
//!
//! A number that breaks the rules is a bug in mdya: the functions return it
//! as an error, and the caller treats it as an unexpected failure.

use nu_ansi_term::Color;
use serde::Serialize;

use super::error_report::{Explanation, McpErrorCode};
use super::operations::{DocumentText, VersionOutput};

mod chars;
mod failure;
mod messages;
mod numbers;
mod results;

#[cfg(test)]
mod tests;

pub(crate) use chars::{escape_direction_characters, replace_controls};
pub(crate) use failure::{FailurePlace, PanicClue, human_failure_place, human_panic};
pub(crate) use messages::{Message, ModelUnknown, Prompt, human_message, human_prompt};
pub(crate) use numbers::UnsafeNumber;
pub(crate) use results::{human_collection_list, human_search, human_settings, human_status};

/// The help as the command line writes it: the text clap made from mdya's
/// own descriptions of the commands, with the control characters and the
/// characters that change the direction of text in each line replaced all
/// the same. The line breaks between the lines are kept.
pub(crate) fn human_help(help: &str) -> String {
    help.split('\n')
        .map(|line| replace_controls(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// mdya's version as the command line writes it: `mdya 1.2.3` and a line
/// break.
pub(crate) fn human_version(version: &VersionOutput) -> String {
    format!("{}\n", replace_controls(&version.version))
}

/// The text of a document as the command line writes it: as it is, without
/// a line break added, and with no character replaced.
pub(crate) fn human_document_text(document: &DocumentText) -> &str {
    &document.text
}

/// `value` as the command line's JSON: one line, without a final line break.
pub(crate) fn cli_json<T: Serialize + ?Sized>(value: &T) -> Result<String, UnsafeNumber> {
    let json = numbers::to_json_string(value)?;
    Ok(chars::escape_direction_characters(&json).into_owned())
}

/// `value` as an MCP tool's result, to pass to the MCP library.
pub(crate) fn mcp_tool_result<T: Serialize + ?Sized>(
    value: &T,
) -> Result<serde_json::Value, UnsafeNumber> {
    numbers::to_json_value(value)
}

/// An MCP tool error, to pass to the MCP library.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct McpToolError {
    pub(crate) code: &'static str,
    /// The explanation, laid out as in the human-readable output, without
    /// color and without replacing any character.
    pub(crate) message: String,
    pub(crate) details: serde_json::Map<String, serde_json::Value>,
}

/// The MCP tool error with `code` and the explanation as its `message`.
pub(crate) fn mcp_tool_error(
    code: McpErrorCode,
    explanation: &Explanation,
) -> Result<McpToolError, UnsafeNumber> {
    numbers::check_json_numbers(&code.details)?;
    Ok(McpToolError {
        code: code.code,
        message: lay_out(explanation, false, &|text: &str| text.to_owned()),
        details: code.details,
    })
}

/// The explanation as the human-readable output: the message, the causes
/// numbered from the nearest, and the lines of the fix, under the headings
/// `Error:`, `Caused by:`, and `How to fix:`, with each cause and each line of
/// the fix on its own line, and without a final line break. An explanation
/// without causes has no `Caused by:` part. With `color`, the headings are
/// red, yellow, and green, and nothing else is colored. Every part of the
/// text has its control characters and the characters that change the
/// direction of text replaced, so a cause stays on one line.
pub(crate) fn human_explanation(explanation: &Explanation, color: bool) -> String {
    lay_out(explanation, color, &|text: &str| {
        replace_controls(text).into_owned()
    })
}

fn lay_out(explanation: &Explanation, color: bool, text: &dyn Fn(&str) -> String) -> String {
    let heading = |name: &str, paint: Color| {
        if color {
            paint.paint(name).to_string()
        } else {
            name.to_owned()
        }
    };
    let mut laid_out = format!(
        "{} {}",
        heading("Error:", Color::Red),
        text(&explanation.message)
    );
    if !explanation.causes.is_empty() {
        laid_out.push_str(&format!("\n\n{}", heading("Caused by:", Color::Yellow)));
        for (number, cause) in explanation.causes.iter().enumerate() {
            laid_out.push_str(&format!("\n    {number}: {}", text(cause)));
        }
    }
    laid_out.push_str(&format!("\n\n{}", heading("How to fix:", Color::Green)));
    for line in &explanation.fix {
        laid_out.push_str(&format!("\n    {}", text(line)));
    }
    laid_out
}
