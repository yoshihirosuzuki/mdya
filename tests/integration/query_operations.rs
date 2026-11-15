//! Searching documents and getting a document on the command line, with the
//! real components: the search modes, the collections and the limit of a
//! search, hits of documents and of chunks, the words that full-text search
//! finds, getting the whole body of a document or the part that its chunks
//! cover, and the errors of these operations.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Output};

use serde_json::{Value, json};

use crate::common::{Outcome, Workspace};

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// Makes a directory collection named `name` on a directory of the same name
/// that holds `files`, updates it, and returns the directory.
fn updated_collection(workspace: &Workspace, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = workspace.dir(name);
    for (file, contents) in files {
        std::fs::write(dir.join(file), contents).unwrap();
    }
    assert_success(&workspace.run(&["collection", "add", name, &text(&dir)]));
    assert_success(&workspace.run(&["update", name]));
    dir
}

/// Makes a raw collection named `name`, and puts `documents`, Markdown, into
/// it under their keys.
fn raw_collection(workspace: &Workspace, name: &str, documents: &[(&str, &str)]) {
    assert_success(&workspace.run(&["collection", "add", name, "--type", "raw"]));
    for &(key, contents) in documents {
        assert_success(&workspace.run_with_stdin(
            &["put", name, key, "--file-type", "md", "-"],
            contents.as_bytes(),
        ));
    }
}

/// The result of `mdya search <query> <options> --json`, which must succeed.
fn search(workspace: &Workspace, query: &str, options: &[&str]) -> Value {
    let mut args = vec!["search", query];
    args.extend_from_slice(options);
    args.push("--json");
    let searched = workspace.run(&args);
    assert_success(&searched);
    serde_json::from_str(&searched.stdout).unwrap()
}

/// The keys of the hits of a full-text search for `query`, in their order.
fn found(workspace: &Workspace, query: &str) -> Vec<String> {
    let result = search(workspace, query, &["--mode", "fts"]);
    keys(&result).into_iter().map(str::to_owned).collect()
}

fn hits(result: &Value) -> &Vec<Value> {
    result["hits"].as_array().unwrap()
}

/// The keys of the hits of a search result, in their order.
fn keys(result: &Value) -> Vec<&str> {
    hits(result)
        .iter()
        .map(|hit| hit["key"].as_str().unwrap())
        .collect()
}

/// The collections of the hits of a search result, in their order.
fn collections(result: &Value) -> Vec<&str> {
    hits(result)
        .iter()
        .map(|hit| hit["collection"].as_str().unwrap())
        .collect()
}

fn scores(result: &Value) -> Vec<f64> {
    hits(result)
        .iter()
        .map(|hit| hit["score"].as_f64().unwrap())
        .collect()
}

/// The names of the fields of a JSON object, sorted.
fn fields(object: &Value) -> Vec<&str> {
    sorted(
        object
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect(),
    )
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items
}

fn assert_best_first(scores: &[f64]) {
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "{scores:?}"
    );
}

/// The score that hybrid search gives the hit `key` for its rank in `result`
/// of one side, counted from 0: 1 / (60 + rank), or 0 when it is not a hit.
fn reciprocal_rank(result: &Value, key: &str) -> f64 {
    keys(result)
        .iter()
        .position(|hit| *hit == key)
        .map_or(0.0, |rank| 1.0 / (60.0 + rank as f64))
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{actual} is not {expected}"
    );
}

/// Runs `mdya get <collection> <key> <options>`.
fn get(workspace: &Workspace, collection: &str, key: &str, options: &[&str]) -> Outcome {
    let mut args = vec!["get", collection, key];
    args.extend_from_slice(options);
    workspace.run(&args)
}

/// What `mdya get <collection> <key> <options>` writes, which must succeed.
fn got(workspace: &Workspace, collection: &str, key: &str, options: &[&str]) -> String {
    let got = get(workspace, collection, key, options);
    assert_success(&got);
    got.stdout
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// Asserts that `outcome` failed with exit code `code`, wrote nothing to
/// stdout, and wrote `message` to stderr.
fn assert_failed(outcome: &Outcome, code: u8, message: &str) {
    assert_eq!(outcome.code, ExitCode::from(code), "{outcome:?}");
    assert_eq!(outcome.stdout, "", "{outcome:?}");
    assert!(outcome.stderr.contains(message), "{outcome:?}");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const RELEASE: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";
const TAGS: &str = "# タグの付け方\n\nタグの名前は、版の番号にします。\n";
const COOKING: &str = "# 料理\n\n玉ねぎを炒めます。\n";

/// A workspace with the collection `notes` of the three documents above,
/// updated. Each document is one chunk.
fn with_notes() -> Workspace {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "notes",
        &[
            ("release.md", RELEASE),
            ("tags.md", TAGS),
            ("cooking.md", COOKING),
        ],
    );
    workspace
}

/// Three chunks, one for each heading, of 30, 30, and 29 bytes.
const SECTIONS: &str =
    "# 一\n\n手順の一つ目。\n\n# 二\n\n手順の二つ目。\n\n# 三\n\n手順の三つ目。\n";

/// A model whose context length, 256 tokens, is shorter than the default
/// model's.
const SECOND_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2";

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_empty_query_or_a_limit_of_0_fails_with_exit_code_2_before_the_collection_is_looked_for() {
    let workspace = Workspace::new();

    let empty =
        ["", " \t", "\u{3000}"].map(|query| workspace.run(&["search", query, "-c", "nothing"]));
    let zero = workspace.run(&["search", "手順", "-n", "0", "-c", "nothing"]);

    for outcome in &empty {
        assert_failed(outcome, 2, "the query is empty");
    }
    assert_failed(&zero, 2, "the limit is zero");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_chunk_range_that_is_not_valid_fails_with_exit_code_2_before_the_collection_is_looked_for() {
    let workspace = Workspace::new();

    let refused = [
        get(&workspace, "nothing", "a.md", &["--end-chunk-index", "0"]),
        get(
            &workspace,
            "nothing",
            "a.md",
            &["--chunk-index", "2", "--end-chunk-index", "1"],
        ),
    ];

    for outcome in &refused {
        assert_failed(outcome, 2, "the chunk range is not valid");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn searching_or_getting_in_a_collection_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    let notes = workspace.dir("notes");
    assert_success(&workspace.run(&["collection", "add", "notes", &text(&notes)]));

    let refused = [
        workspace.run(&["search", "手順", "-c", "nothing"]),
        workspace.run(&["search", "手順", "-c", "notes,nothing"]),
        get(&workspace, "nothing", "a.md", &[]),
    ];

    for outcome in &refused {
        assert_failed(outcome, 1, "the collection \"nothing\" does not exist");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn on_an_index_without_documents_a_full_text_search_finds_nothing_and_getting_fails_with_exit_code_1()
 {
    let workspace = Workspace::new();
    let notes = workspace.dir("notes");
    std::fs::write(notes.join("a.md"), "# 手順\n").unwrap();
    assert_success(&workspace.run(&["collection", "add", "notes", &text(&notes)]));

    let searched = workspace.run(&["search", "手順", "--mode", "fts", "--json"]);
    let refused = get(&workspace, "notes", "a.md", &[]);

    assert_success(&searched);
    let result: Value = serde_json::from_str(&searched.stdout).unwrap();
    assert_eq!(result["hits"], json!([]));
    assert_eq!(result["hit_count"], 0);
    assert_failed(
        &refused,
        1,
        "the document \"a.md\" does not exist in the collection \"notes\"",
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_search_returns_its_input_and_its_hits_best_first_as_json() {
    let workspace = with_notes();

    let result = search(&workspace, "タグ", &[]);

    assert_eq!(
        fields(&result),
        [
            "collections",
            "hit_count",
            "hits",
            "limit",
            "mode",
            "query",
            "unit",
            "warnings"
        ]
    );
    assert_eq!(result["query"], "タグ");
    assert_eq!(result["mode"], "hybrid");
    assert_eq!(result["unit"], "document");
    assert_eq!(result["collections"], json!([]));
    assert_eq!(result["limit"], 20);
    assert_eq!(result["warnings"], json!([]));
    assert_eq!(result["hit_count"], hits(&result).len());
    assert_eq!(
        sorted(keys(&result)),
        ["cooking.md", "release.md", "tags.md"]
    );
    for hit in hits(&result) {
        assert_eq!(
            fields(hit),
            [
                "chunk_count",
                "chunk_index",
                "collection",
                "key",
                "score",
                "snippet"
            ]
        );
        assert_eq!(hit["collection"], "notes");
        assert_eq!(hit["chunk_index"], 0);
        assert_eq!(hit["chunk_count"], 1);
    }
    assert_best_first(&scores(&result));
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn each_search_mode_finds_and_scores_hits_by_its_own_rule() {
    let workspace = with_notes();
    let every_key = ["cooking.md", "release.md", "tags.md"];

    let full_text = search(&workspace, "タグ", &["--mode", "fts"]);
    let vector = search(&workspace, "タグ", &["--mode", "vector"]);
    let hybrid = search(&workspace, "タグ", &["--mode", "hybrid"]);
    let in_no_document =
        ["fts", "vector", "hybrid"].map(|mode| search(&workspace, "自転車", &["--mode", mode]));

    // Full-text search: the documents with the word, best first.
    assert_eq!(full_text["mode"], "fts");
    assert_eq!(sorted(keys(&full_text)), ["release.md", "tags.md"]);
    assert!(scores(&full_text).iter().all(|score| *score > 0.0));
    assert_best_first(&scores(&full_text));
    // A short chunk is its snippet, each run of line breaks a space.
    for hit in hits(&full_text) {
        let expected = match hit["key"].as_str().unwrap() {
            "release.md" => "# リリースの手順 タグを打つ前に、変更履歴を確かめます。 ",
            _ => "# タグの付け方 タグの名前は、版の番号にします。 ",
        };
        assert_eq!(hit["snippet"], expected);
    }
    // Vector search: every document, best first, with a score from 0 to 1.
    assert_eq!(vector["mode"], "vector");
    assert_eq!(sorted(keys(&vector)), every_key);
    assert!(
        scores(&vector)
            .iter()
            .all(|score| (0.0..=1.0).contains(score))
    );
    assert_best_first(&scores(&vector));
    // Hybrid search: the reciprocal ranks of both sides added.
    assert_eq!(hybrid["mode"], "hybrid");
    assert_eq!(sorted(keys(&hybrid)), every_key);
    for hit in hits(&hybrid) {
        let key = hit["key"].as_str().unwrap();
        assert_close(
            hit["score"].as_f64().unwrap(),
            reciprocal_rank(&full_text, key) + reciprocal_rank(&vector, key),
        );
    }
    assert_best_first(&scores(&hybrid));
    // A word that no document has: no full-text hits, and every document
    // for the other two.
    let [no_word_full_text, no_word_vector, no_word_hybrid] = &in_no_document;
    assert_eq!(hits(no_word_full_text).len(), 0);
    assert_eq!(sorted(keys(no_word_vector)), every_key);
    assert_eq!(sorted(keys(no_word_hybrid)), every_key);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_search_looks_in_the_given_collections_only() {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "notes",
        &[("a.md", "# 手順\n\nノートの手順。\n")],
    );
    updated_collection(&workspace, "work", &[("b.md", "# 手順\n\n仕事の手順。\n")]);
    raw_collection(&workspace, "memo", &[("c.md", "# 手順\n\nメモの手順。\n")]);

    let every = search(&workspace, "手順", &["--mode", "fts"]);
    let notes = ["fts", "vector", "hybrid"]
        .map(|mode| search(&workspace, "手順", &["--mode", mode, "-c", "notes"]));
    let joined = search(&workspace, "手順", &["--mode", "fts", "-c", "notes,memo"]);
    let repeated = search(
        &workspace,
        "手順",
        &["--mode", "fts", "--collection", "notes", "-c", "memo"],
    );

    assert_eq!(sorted(collections(&every)), ["memo", "notes", "work"]);
    assert_eq!(every["collections"], json!([]));
    for result in &notes {
        assert_eq!(collections(result), ["notes"]);
        assert_eq!(result["collections"], json!(["notes"]));
    }
    for result in [&joined, &repeated] {
        assert_eq!(sorted(collections(result)), ["memo", "notes"]);
        assert_eq!(result["collections"], json!(["notes", "memo"]));
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_search_returns_no_more_hits_than_its_limit() {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "notes",
        &[
            ("a.md", "# 手順\n\n一つ目の手順。\n"),
            ("b.md", "# 手順\n\n二つ目の手順。\n"),
            ("c.md", "# 手順\n\n三つ目の手順。\n"),
        ],
    );

    let every = search(&workspace, "手順", &["--mode", "fts"]);
    let two = search(&workspace, "手順", &["--mode", "fts", "-n", "2"]);
    let one = ["vector", "hybrid"]
        .map(|mode| search(&workspace, "手順", &["--mode", mode, "--limit", "1"]));

    assert_eq!(hits(&every).len(), 3);
    assert_eq!(two["limit"], 2);
    assert_eq!(two["hit_count"], 2);
    assert_eq!(keys(&two), keys(&every)[..2]);
    for result in &one {
        assert_eq!(result["limit"], 1);
        assert_eq!(result["hit_count"], 1);
        assert_eq!(hits(result).len(), 1);
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_search_by_chunk_returns_each_matching_chunk_as_a_hit() {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "notes",
        &[
            ("sections.md", SECTIONS),
            ("other.md", "# 他\n\n別の手順。\n"),
        ],
    );

    let by_chunk = search(&workspace, "手順", &["--mode", "fts", "--unit", "chunk"]);
    let by_document = search(&workspace, "手順", &["--mode", "fts", "--unit", "document"]);

    assert_eq!(by_chunk["unit"], "chunk");
    assert_eq!(by_chunk["hit_count"], 4);
    let chunks: Vec<(&str, u64, u64)> = hits(&by_chunk)
        .iter()
        .map(|hit| {
            (
                hit["key"].as_str().unwrap(),
                hit["chunk_index"].as_u64().unwrap(),
                hit["chunk_count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sorted(chunks),
        [
            ("other.md", 0, 1),
            ("sections.md", 0, 3),
            ("sections.md", 1, 3),
            ("sections.md", 2, 3)
        ]
    );
    assert_eq!(by_document["unit"], "document");
    let documents: Vec<(&str, u64)> = hits(&by_document)
        .iter()
        .map(|hit| {
            (
                hit["key"].as_str().unwrap(),
                hit["chunk_count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(sorted(documents), [("other.md", 1), ("sections.md", 3)]);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_search_without_json_writes_its_hits_for_people() {
    let workspace = with_notes();

    let found = workspace.run(&["search", "変更履歴", "--mode", "fts"]);
    let nothing = workspace.run(&["search", "自転車", "--mode", "fts"]);

    assert_success(&found);
    let lines: Vec<&str> = found.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{found:?}");
    assert_eq!(lines[0], "1. notes  release.md");
    assert!(
        lines[1].starts_with("   score ") && lines[1].ends_with(", chunk 0 of 1"),
        "{found:?}"
    );
    assert_eq!(
        lines[2],
        "   # リリースの手順 タグを打つ前に、変更履歴を確かめます。 "
    );
    assert_success(&nothing);
    assert_eq!(nothing.stdout, "(no hits)\n");
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn full_text_search_splits_japanese_by_the_dictionary_and_letters_from_digits_and_symbols() {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "words",
        &[
            (
                "compound.md",
                "# 設定\n\nモデルキャッシュの場所を変えます。\n",
            ),
            ("coined.md", "# 造語\n\nポヨムキャッシュを使います。\n"),
            ("type.md", "# 型\n\n値は u32 で持ちます。\n"),
            ("language.md", "# 言語\n\nC++ で書きます。\n"),
            ("record.md", "# 記録\n\n彼は昨日走った。\n"),
        ],
    );

    // A compound of words in the dictionary is split into them; one with a
    // part that is not in the dictionary is one word.
    assert_eq!(found(&workspace, "モデル"), ["compound.md"]);
    assert_eq!(found(&workspace, "キャッシュ"), ["compound.md"]);
    assert_eq!(found(&workspace, "ポヨムキャッシュ"), ["coined.md"]);
    // Letters and digits are split where they change, and a symbol in a
    // word of letters is a word.
    assert_eq!(found(&workspace, "u"), ["type.md"]);
    assert_eq!(found(&workspace, "32"), ["type.md"]);
    assert_eq!(found(&workspace, "++"), ["language.md"]);
    // Japanese conjugations are told apart.
    assert_eq!(found(&workspace, "走った"), ["record.md"]);
    assert_eq!(found(&workspace, "走る"), Vec::<String>::new());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn full_text_search_takes_characters_that_differ_only_in_width_case_accents_or_english_form_as_the_same()
 {
    let workspace = Workspace::with_real_model();
    updated_collection(
        &workspace,
        "words",
        &[
            ("width.md", "# 幅\n\nﾓﾃﾞﾙとＡＢＣと①を使います。\n"),
            (
                "english.md",
                "# English\n\nThe job was Running at the Café.\n",
            ),
        ],
    );

    for query in ["モデル", "ﾓﾃﾞﾙ", "abc", "ＡＢＣ", "1", "①"] {
        assert_eq!(found(&workspace, query), ["width.md"], "{query}");
    }
    for query in ["run", "RUNNING", "cafe", "CAFÉ"] {
        assert_eq!(found(&workspace, query), ["english.md"], "{query}");
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn full_text_search_leaves_out_stop_words_and_words_of_255_bytes_or_more() {
    let workspace = Workspace::with_real_model();
    let bytes_254 = "b".repeat(254);
    let bytes_255 = "c".repeat(255);
    let document_254 = format!("# 長さ\n\n{bytes_254}\n");
    let document_255 = format!("# 長さ\n\n{bytes_255}\n");
    updated_collection(
        &workspace,
        "words",
        &[
            ("negation.md", "# 消す\n\n古い版は削除しない。\n"),
            (
                "english.md",
                "# English\n\nThe job was Running at the Café.\n",
            ),
            ("254.md", document_254.as_str()),
            ("255.md", document_255.as_str()),
            ("heading.md", "# 長さ\n"),
        ],
    );

    // Each stop word is in a document that other words find.
    assert_eq!(found(&workspace, "古い"), ["negation.md"]);
    assert_eq!(found(&workspace, "job"), ["english.md"]);
    for query in ["は", "ない", "the"] {
        assert_eq!(found(&workspace, query), Vec::<String>::new(), "{query}");
    }
    let negated = search(&workspace, "削除しない", &["--mode", "fts"]);
    let not_negated = search(&workspace, "削除し", &["--mode", "fts"]);
    assert_eq!(keys(&negated), ["negation.md"]);
    assert_eq!(hits(&negated), hits(&not_negated));
    // A word of 254 bytes finds its document, and one of 255 bytes finds
    // nothing. A run of one letter is one word: ten `b`s do not find
    // `254.md`.
    assert_eq!(found(&workspace, &bytes_254), ["254.md"]);
    assert_eq!(found(&workspace, &"b".repeat(10)), Vec::<String>::new());
    assert_eq!(found(&workspace, &bytes_255), Vec::<String>::new());
    // Nor is a word of 255 bytes a word of the document: with as many words
    // as the document of the heading alone, it has the same BM25 value for
    // the heading, while the one more word of `254.md` lowers its value.
    let heading = search(&workspace, "長さ", &["--mode", "fts"]);
    let score_of = |key: &str| {
        let hit = hits(&heading).iter().find(|hit| hit["key"] == key);
        hit.unwrap()["score"].as_f64().unwrap()
    };
    assert_eq!(sorted(keys(&heading)), ["254.md", "255.md", "heading.md"]);
    assert_eq!(score_of("255.md"), score_of("heading.md"));
    assert!(score_of("254.md") < score_of("heading.md"));
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_vector_or_hybrid_search_fails_with_exit_code_1_when_the_model_cannot_be_used_but_full_text_search_and_get_work()
 {
    let workspace = with_notes();
    let run_without_the_model = |args: &[&str]| {
        workspace
            .executable_without_the_model()
            .args(args)
            .output()
            .unwrap()
    };

    let refused = [
        run_without_the_model(&["search", "タグ"]),
        run_without_the_model(&["search", "タグ", "--mode", "vector"]),
    ];
    let full_text = run_without_the_model(&["search", "タグ", "--mode", "fts", "--json"]);
    let got = run_without_the_model(&["get", "notes", "release.md"]);

    for output in &refused {
        let stderr = stderr(output);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert_eq!(stdout(output), "", "{stderr}");
        assert!(
            stderr.contains("the embedding model cannot be used"),
            "{stderr}"
        );
        // The cause names the request for the model that failed.
        assert!(stderr.contains("Caused by:"), "{stderr}");
        assert!(stderr.contains("https://huggingface.co/"), "{stderr}");
    }
    assert_eq!(full_text.status.code(), Some(0), "{}", stderr(&full_text));
    let result: Value = serde_json::from_slice(&full_text.stdout).unwrap();
    assert_eq!(sorted(keys(&result)), ["release.md", "tags.md"]);
    assert_eq!(got.status.code(), Some(0), "{}", stderr(&got));
    assert_eq!(stdout(&got), RELEASE);
}

#[test]
#[ignore = "runs a real embedding model, not the default one; run it with `just integration`"]
fn a_query_longer_than_the_model_context_length_fails_a_vector_or_hybrid_search_with_exit_code_1() {
    let workspace = Workspace::with_real_model();
    assert_success(&workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]));
    // For this model, each `a` and each `,` is one token: the long query, of
    // 600 characters, is 400 tokens, and the short one 100.
    let long = "a, ".repeat(200);
    let short = "a, ".repeat(50);

    let refused =
        ["hybrid", "vector"].map(|mode| workspace.run(&["search", &long, "--mode", mode]));
    let accepted = [
        workspace.run(&["search", &long, "--mode", "fts"]),
        workspace.run(&["search", &short, "--mode", "hybrid"]),
        workspace.run(&["search", &short, "--mode", "vector"]),
    ];

    for outcome in &refused {
        assert_failed(
            outcome,
            1,
            "the query is longer than the embedding model's context length",
        );
    }
    for outcome in &accepted {
        assert_success(outcome);
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn getting_a_document_returns_a_markdown_body_as_it_is_and_the_text_of_a_pdf() {
    let workspace = Workspace::with_real_model();
    let body = "# 本文\r\n\r\nタブ\tと、エスケープ \u{1b}[31m と、改行のない終わり";
    let pdf = workspace.dir("notes").join("english.pdf");
    std::fs::copy(fixture("pdfs/english.pdf"), pdf).unwrap();
    updated_collection(&workspace, "notes", &[("body.md", body)]);

    let markdown = got(&workspace, "notes", "body.md", &[]);
    let pdf = got(&workspace, "notes", "english.pdf", &[]);

    assert_eq!(markdown, body);
    // The title line of the file, which the unit tests of PDF look for too.
    assert!(pdf.contains("mdya PDF ingest smoke fixture"), "{pdf}");
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn getting_chunks_returns_the_part_of_the_body_that_they_cover() {
    let workspace = Workspace::with_real_model();
    let long = format!("# 長い段落\n\n{}\n", "段落の文。".repeat(200));
    updated_collection(
        &workspace,
        "notes",
        &[("sections.md", SECTIONS), ("long.md", long.as_str())],
    );
    let sections = |options: &[&str]| got(&workspace, "notes", "sections.md", options);

    assert_eq!(
        sections(&["--chunk-index", "1"]),
        "# 二\n\n手順の二つ目。\n\n"
    );
    assert_eq!(
        sections(&["--chunk-index", "0", "--end-chunk-index", "1"]),
        "# 一\n\n手順の一つ目。\n\n# 二\n\n手順の二つ目。\n\n"
    );
    assert_eq!(
        sections(&["--chunk-index", "2", "--end-chunk-index", "2"]),
        "# 三\n\n手順の三つ目。\n"
    );
    assert_eq!(
        sections(&["--chunk-index", "1", "--end-chunk-index", "9"]),
        "# 二\n\n手順の二つ目。\n\n# 三\n\n手順の三つ目。\n"
    );
    assert_failed(
        &get(&workspace, "notes", "sections.md", &["--chunk-index", "3"]),
        1,
        "the chunk does not exist",
    );
    // Chunks that overlap: a range has each character once.
    let result = search(&workspace, "段落", &["--mode", "fts"]);
    assert_eq!(keys(&result), ["long.md"]);
    let count = hits(&result)[0]["chunk_count"].as_u64().unwrap();
    assert!(count >= 2, "{result}");
    let chunks: Vec<String> = (0..count)
        .map(|index| {
            got(
                &workspace,
                "notes",
                "long.md",
                &["--chunk-index", &index.to_string()],
            )
        })
        .collect();
    assert!(chunks.iter().all(|chunk| long.contains(chunk.as_str())));
    assert!(chunks.iter().map(String::len).sum::<usize>() > long.len());
    let last = (count - 1).to_string();
    assert_eq!(
        got(
            &workspace,
            "notes",
            "long.md",
            &["--chunk-index", "0", "--end-chunk-index", &last]
        ),
        long
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_is_got_from_the_index_until_an_update_deletes_it() {
    let workspace = Workspace::with_real_model();
    let notes = updated_collection(&workspace, "notes", &[("release.md", RELEASE)]);
    std::fs::remove_file(notes.join("release.md")).unwrap();

    let before_the_update = got(&workspace, "notes", "release.md", &[]);
    assert_success(&workspace.run(&["update", "notes"]));
    let after_the_update = get(&workspace, "notes", "release.md", &[]);

    assert_eq!(before_the_update, RELEASE);
    assert_failed(
        &after_the_update,
        1,
        "the document \"release.md\" does not exist in the collection \"notes\"",
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn the_output_size_limit_refuses_a_larger_text_unless_it_is_lifted() {
    let workspace = Workspace::with_real_model();
    updated_collection(&workspace, "notes", &[("sections.md", SECTIONS)]);
    let limit = |bytes: &str| {
        assert_success(&workspace.run(&["config", "set", "get.cli_limit_bytes", bytes]));
    };

    limit("29");
    let refused = [
        (
            get(&workspace, "notes", "sections.md", &[]),
            "the document's body is too large to return: 89 bytes, over the limit of 29 bytes",
        ),
        (
            get(&workspace, "notes", "sections.md", &["--chunk-index", "0"]),
            "the chunk is too large to return: 30 bytes, over the limit of 29 bytes",
        ),
        (
            get(
                &workspace,
                "notes",
                "sections.md",
                &["--chunk-index", "1", "--end-chunk-index", "2"],
            ),
            "the chunk range is too large to return: 59 bytes, over the limit of 29 bytes",
        ),
    ];
    let at_the_limit = got(&workspace, "notes", "sections.md", &["--chunk-index", "2"]);
    let lifted = got(&workspace, "notes", "sections.md", &["--no-size-limit"]);
    limit("0");
    let without_a_limit = got(&workspace, "notes", "sections.md", &[]);

    for (outcome, message) in &refused {
        assert_failed(outcome, 1, message);
    }
    assert_eq!(at_the_limit, "# 三\n\n手順の三つ目。\n");
    assert_eq!(lifted, SECTIONS);
    assert_eq!(without_a_limit, SECTIONS);
}
