//! Cutting the snippet of a hit out of a chunk, around the first word of the
//! query that the chunk contains.

use std::collections::HashSet;

use lance_index::scalar::inverted::tokenizer::document_tokenizer::LanceTokenizer;
use lance_tokenizer::TokenStream;
use lancedb::index::scalar::FtsIndexBuilder;
use tokio::sync::OnceCell;

use super::run_blocking;
use crate::types::BoxError;

#[cfg(test)]
mod tests;

const SNIPPET_CHARS: usize = 60;
const CHARS_BEFORE_THE_WORD: usize = 30;
const CUT_MARK: char = '…';

/// Builds the tokenizer of the full-text search settings once, on first use,
/// because building it loads the dictionary. A failure is not kept, so a later
/// search tries again.
pub(super) struct Tokenizers {
    settings: fn() -> FtsIndexBuilder,
    built: OnceCell<Box<dyn LanceTokenizer>>,
}

impl Tokenizers {
    pub(super) fn new(settings: fn() -> FtsIndexBuilder) -> Self {
        Self {
            settings,
            built: OnceCell::new(),
        }
    }

    /// A tokenizer for one search.
    pub(super) async fn get(&self) -> Result<Box<dyn LanceTokenizer>, BoxError> {
        let built = self.built.get_or_try_init(|| self.build()).await?;
        Ok(built.clone())
    }

    async fn build(&self) -> Result<Box<dyn LanceTokenizer>, BoxError> {
        let settings = self.settings;
        Ok(run_blocking(move || settings().build()).await??)
    }
}

/// The words a search looks for, as the full-text search index sees them.
pub(super) fn query_words(tokenizer: &mut Box<dyn LanceTokenizer>, query: &str) -> HashSet<String> {
    let mut words = HashSet::new();
    let mut tokens = tokenizer.token_stream_for_search(query);
    while tokens.advance() {
        words.insert(tokens.token().text.clone());
    }
    words
}

/// The snippet of `chunk`: the text around the first of `words` it contains,
/// or its start when it contains none.
pub(super) fn snippet(
    tokenizer: &mut Box<dyn LanceTokenizer>,
    chunk: &str,
    words: &HashSet<String>,
) -> String {
    let word_at = first_word_at(tokenizer, chunk, words);
    // The offset of a token is a byte position in `chunk` as it was given,
    // before the tokenizer normalized its characters. If one ever fell inside
    // a character, the snippet would start at the start of the chunk.
    let chars_before = word_at
        .and_then(|at| chunk.get(..at))
        .map_or(0, |before| before.chars().count());
    cut(chunk, chars_before.saturating_sub(CHARS_BEFORE_THE_WORD))
}

fn first_word_at(
    tokenizer: &mut Box<dyn LanceTokenizer>,
    chunk: &str,
    words: &HashSet<String>,
) -> Option<usize> {
    let mut tokens = tokenizer.token_stream_for_doc(chunk);
    while tokens.advance() {
        if words.contains(&tokens.token().text) {
            return Some(tokens.token().offset_from);
        }
    }
    None
}

/// Up to [`SNIPPET_CHARS`] characters of `chunk` from the character numbered
/// `start`, with each run of line breaks as one space and a mark on each side
/// that was cut. The limit counts the characters before the line breaks are
/// replaced.
fn cut(chunk: &str, start: usize) -> String {
    let mut rest = chunk.chars().skip(start);
    let kept: String = rest.by_ref().take(SNIPPET_CHARS).collect();
    let mut snippet = String::with_capacity(kept.len());
    if start > 0 {
        snippet.push(CUT_MARK);
    }
    push_with_spaces_for_line_breaks(&mut snippet, &kept);
    if rest.next().is_some() {
        snippet.push(CUT_MARK);
    }
    snippet
}

fn push_with_spaces_for_line_breaks(snippet: &mut String, text: &str) {
    let mut in_line_breaks = false;
    for character in text.chars() {
        let is_line_break = matches!(character, '\r' | '\n');
        if !is_line_break {
            snippet.push(character);
        } else if !in_line_breaks {
            snippet.push(' ');
        }
        in_line_breaks = is_line_break;
    }
}
