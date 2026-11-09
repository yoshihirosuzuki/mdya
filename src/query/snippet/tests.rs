use lance_index::scalar::inverted::tokenizer::document_tokenizer::LanceTokenizer;

use super::{cut, query_words, snippet};
use crate::index::fts_index_params;
use crate::test_support::prepare_full_text_search;

async fn tokenizer() -> Box<dyn LanceTokenizer> {
    prepare_full_text_search().await;
    fts_index_params().build().unwrap()
}

async fn snippet_of(chunk: &str, query: &str) -> String {
    let mut tokenizer = tokenizer().await;
    let words = query_words(&mut tokenizer, query);
    snippet(&mut tokenizer, chunk, &words)
}

fn repeated(character: char, count: usize) -> String {
    std::iter::repeat_n(character, count).collect()
}

#[test]
fn a_short_chunk_is_shown_whole_without_marks() {
    assert_eq!(cut("short text", 0), "short text");
}

#[test]
fn sixty_characters_are_not_cut_but_sixty_one_are() {
    let sixty = repeated('a', 60);
    assert_eq!(cut(&sixty, 0), sixty);
    let sixty_one = repeated('a', 61);
    assert_eq!(cut(&sixty_one, 0), format!("{sixty}…"));
}

#[test]
fn a_cut_at_the_start_gets_a_mark_there() {
    let text = format!("{}{}", repeated('a', 5), repeated('b', 10));
    assert_eq!(cut(&text, 5), format!("…{}", repeated('b', 10)));
}

#[test]
fn characters_are_counted_as_code_points() {
    let text = repeated('語', 61);
    assert_eq!(cut(&text, 0), format!("{}…", repeated('語', 60)));
}

#[test]
fn each_run_of_line_breaks_becomes_one_space() {
    assert_eq!(cut("a\nb\r\nc\rd\n\n\r\ne", 0), "a b c d e");
}

#[test]
fn the_limit_counts_the_line_breaks_before_they_are_replaced() {
    let text = format!("{}\n\n{}", repeated('a', 58), repeated('b', 5));
    assert_eq!(cut(&text, 0), format!("{} …", repeated('a', 58)));
}

#[tokio::test]
async fn a_chunk_without_the_words_is_shown_from_its_start() {
    let chunk = format!("{} end", repeated('x', 70));
    let expected = format!("{}…", repeated('x', 60));
    assert_eq!(snippet_of(&chunk, "absent").await, expected);
}

#[tokio::test]
async fn a_word_within_the_first_thirty_characters_is_shown_from_the_start() {
    let shown = snippet_of("the release steps are here", "release").await;
    assert_eq!(shown, "the release steps are here");
}

#[tokio::test]
async fn the_snippet_starts_thirty_characters_before_the_first_word() {
    let before = format!("{} ", repeated('x', 39));
    let chunk = format!("{before}release {}", repeated('y', 70));
    let shown = snippet_of(&chunk, "release").await;
    let expected_body: String = chunk.chars().skip(10).take(60).collect();
    assert_eq!(shown, format!("…{expected_body}…"));
}

#[tokio::test]
async fn the_first_word_of_the_chunk_decides_whichever_comes_first_in_the_query() {
    let before = format!("{} ", repeated('x', 39));
    let chunk = format!("{before}alpha and beta");
    let shown = snippet_of(&chunk, "beta alpha").await;
    let expected_body: String = chunk.chars().skip(10).take(60).collect();
    assert_eq!(shown, format!("…{expected_body}"));
}

#[tokio::test]
async fn a_word_is_found_in_its_other_forms_and_in_japanese() {
    let padding = repeated('x', 39);
    let chunk = format!("{padding} Running the job");
    let shown = snippet_of(&chunk, "run").await;
    assert!(shown.contains("Running"), "{shown}");

    let shown = snippet_of("今日はリリースの手順を確かめます", "手順").await;
    assert_eq!(shown, "今日はリリースの手順を確かめます");
}

#[tokio::test]
async fn a_query_of_only_stop_words_has_no_words() {
    let mut tokenizer = tokenizer().await;
    assert!(query_words(&mut tokenizer, "the a you").is_empty());
}
