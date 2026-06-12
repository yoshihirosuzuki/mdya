//! Empirical verification of lindera/ipadic placement prerequisites.
//!
//! Gated behind `#[ignore]` and run via `just lindera-ipadic-verify`. Each
//! test isolates one axis of the placement question (= what does Lance
//! actually require to load its `lindera/ipadic` tokenizer, and which
//! placement strategy keeps mdya inside its own `~/.mdya/` namespace).
//!
//! Findings are printed via `[ipadic finding] ...` lines. Every test
//! that touches `language_model_home` redirects Lance to a tempdir via
//! the shared `ScopedLanceLanguageModelHome` guard from
//! `tests/common/mod.rs`, so the user's `<system data dir>/lance/...`
//! tree is never written to.
//!
//! Do not relax the `#[ignore]` attribute without re-evaluating the
//! placement prerequisites.

mod common;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{
    FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
    TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::Schema;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{Connection, Table};
use tempfile::TempDir;

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};
use mdya::store::{CHUNKS_TABLE_NAME, chunks_schema};

const VECTOR_DIM: i32 = 256;
const MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";

// Mirrors `lance_index::scalar::inverted::tokenizer::language_model_home`
// (lance-index 6.0.0, src/scalar/inverted/tokenizer.rs:430). Inlined here
// because the function is invoked transitively at `create_index` time and
// is not part of lance-index's public surface — verification has to
// reconstruct the same path the engine would compute internally.
//
// The env-key name itself lives in `mdya::store::lance_lm`; this file
// keeps the default-directory mirror because it is purely a
// verification-only piece of Lance internals (`tests/common` holds
// the env-mutation guard, not the path-resolution mirror).
const LANCE_LANGUAGE_MODEL_DEFAULT_DIRECTORY: &str = "lance/language_models";

fn language_model_home() -> Option<PathBuf> {
    match env::var(mdya::store::lance_lm::LANCE_LANGUAGE_MODEL_HOME_ENV_KEY) {
        Ok(p) => Some(PathBuf::from(p)),
        Err(_) => dirs::data_local_dir().map(|p| p.join(LANCE_LANGUAGE_MODEL_DEFAULT_DIRECTORY)),
    }
}

fn schema_arc() -> Arc<Schema> {
    Arc::new(chunks_schema(VECTOR_DIM, MODEL_ID))
}

async fn fresh_connection(tmp: &TempDir) -> Result<Connection> {
    let path = tmp.path().join("index");
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_str().context("UTF-8 tempdir path")?;
    Ok(lancedb::connect(path_str).execute().await?)
}

async fn fresh_empty_table(tmp: &TempDir) -> Result<Table> {
    let db = fresh_connection(tmp).await?;
    let tbl = db
        .create_empty_table(CHUNKS_TABLE_NAME, schema_arc())
        .execute()
        .await?;
    Ok(tbl)
}

fn deterministic_embedding(seed: u64) -> FixedSizeListArray {
    let values = Float32Builder::with_capacity(VECTOR_DIM as usize);
    let mut builder = FixedSizeListBuilder::new(values, VECTOR_DIM);
    let v: Vec<f32> = (0..VECTOR_DIM)
        .map(|i| ((seed as f32) + (i as f32)).sin())
        .collect();
    builder.values().append_slice(&v);
    builder.append(true);
    builder.finish()
}

fn build_row_batch(seed: u64, body: &str) -> RecordBatch {
    let schema = schema_arc();
    let collection = StringArray::from(vec!["notes"]);
    let path = StringArray::from(vec![format!("doc-{seed}.md")]);
    let chunk_sequence = UInt32Array::from(vec![0_u32]);
    let body = StringArray::from(vec![body.to_string()]);
    let embedding = deterministic_embedding(seed);
    let modified_at = TimestampMicrosecondArray::from(vec![1_700_000_000_000_000_i64])
        .with_timezone("UTC".to_string());
    let source_hash = StringArray::from(vec![format!("{:0>64}", seed)]);
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(collection),
            Arc::new(path),
            Arc::new(chunk_sequence),
            Arc::new(body),
            Arc::new(embedding),
            Arc::new(modified_at),
            Arc::new(source_hash),
        ],
    )
    .unwrap_or_else(|e| panic!("RecordBatch::try_new failed (schema / columns mismatch?): {e}"))
}

async fn insert_with_bodies(tbl: &Table, rows: &[(u64, &str)]) -> Result<()> {
    let schema = schema_arc();
    let batches: Vec<_> = rows
        .iter()
        .map(|(s, b)| Ok(build_row_batch(*s, b)))
        .collect();
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(batches, schema));
    tbl.add(reader).execute().await?;
    Ok(())
}

fn finding(name: &str, msg: impl std::fmt::Display) {
    println!("[ipadic finding] {name}: {msg}");
}

/// Build the standard `<home>/lindera/ipadic/` directory underneath a
/// fresh tempdir-backed Lance language model home, and write the tiny
/// `config.yml` that points to the embedded IPADIC dictionary. Reuses
/// the production constant so a future tweak to the YAML stays in one
/// place (`src/store/lance_lm.rs`).
fn prepare_lindera_ipadic_home(tmp: &TempDir) -> Result<PathBuf> {
    let home = tmp.path().to_path_buf();
    let dir = home.join("lindera").join("ipadic");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("config.yml"),
        mdya::store::lance_lm::LINDERA_IPADIC_CONFIG_YML,
    )?;
    Ok(home)
}

// ───────────────────────────── axis A ──────────────────────────────────────

/// Pure observation of the path Lance computes for `lindera/ipadic` on this
/// machine. Cross-checks our re-implementation of `language_model_home()`
/// against the env var override behaviour. Read-only; no writes anywhere.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_a_observed_language_model_home() -> Result<()> {
    let env_key = mdya::store::lance_lm::LANCE_LANGUAGE_MODEL_HOME_ENV_KEY;
    let env_override = env::var(env_key).ok();
    let home = language_model_home();
    let ipadic_dir = home.as_ref().map(|h| h.join("lindera").join("ipadic"));
    finding(
        "axis_a_env_var_present",
        format!("{env_key} env var = {:?}", env_override),
    );
    finding(
        "axis_a_resolved_home",
        format!("language_model_home() = {:?}", home),
    );
    finding(
        "axis_a_resolved_ipadic_dir",
        format!("expected ipadic dir = {:?}", ipadic_dir),
    );
    Ok(())
}

// ───────────────────────────── axis C — fallback failure ───────────────────

/// Does `create_index(FTS, "lindera/ipadic")` succeed when the directory
/// exists but contains no `config.yml`? The Lance source path hits the
/// fallback to `LinderaTokenizer::new()` → `TokenizerBuilder::new()` →
/// empty config → build() failure. This failure mode is independent of
/// the `lindera/ipadic` cargo feature.
///
/// Redirected to a tempdir via env override so the user's
/// `<system data dir>` is not touched.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_c_empty_directory_fallback_fails() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let home_tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(home_tmp.path());
    let ipadic_dir = home_tmp.path().join("lindera").join("ipadic");
    std::fs::create_dir_all(&ipadic_dir)?;

    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_with_bodies(&tbl, &[(1, "hello world")]).await?;
    let attempt = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await;
    match attempt {
        Ok(()) => finding(
            "axis_c_empty_dir_fallback",
            "create_index unexpectedly succeeded on empty dir",
        ),
        Err(e) => finding(
            "axis_c_empty_dir_fallback",
            format!("create_index FAILED: {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis E — env redirect works ─────────────────

/// Setting `LANCE_LANGUAGE_MODEL_HOME` to a tempdir redirects Lance's
/// `language_model_home()` away from the OS default. Confirmed by reading
/// back the path Lance mentions in its error message when the redirected
/// config.yml is missing.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_e_env_var_override_redirects_lance() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let custom_home_tmp = TempDir::new()?;
    let custom_home = custom_home_tmp.path().to_path_buf();
    let _guard = ScopedLanceLanguageModelHome::set(&custom_home);
    // Intentionally leave both the directory and config.yml missing so
    // Lance's loader fails at `p.is_dir()` and the resulting "Invalid
    // directory path: <path>" error embeds the path it consulted — that
    // path is the redirect signal we care about.

    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_with_bodies(&tbl, &[(1, "hello world")]).await?;
    let attempt = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await;
    let attempt_repr = match &attempt {
        Ok(()) => "Ok".to_string(),
        Err(e) => format!("Err: {e}"),
    };
    let custom_home_str = custom_home.display().to_string();
    let redirected = attempt_repr.contains(&custom_home_str);
    finding(
        "axis_e_env_var_redirect",
        format!(
            "custom_home = {:?}, error mentions custom path = {redirected}, full attempt = {attempt_repr}",
            custom_home
        ),
    );
    Ok(())
}

// ───────────────────────────── axis F — pre-tokenised alternative ──────────

/// Considered alternative: pre-tokenise on the mdya side and feed
/// whitespace-separated tokens to Lance's built-in `whitespace`
/// tokenizer. Bypasses `language_model_home` entirely (no foreign disk
/// write, no env var) but requires a schema change to retain the
/// original body for snippet display and produces no morphological
/// fallback for unsegmented queries (= "東京駅" misses pre-tokenised
/// "東京 駅"). Recorded for ADR completeness.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_f_pretokenised_whitespace_fts() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_with_bodies(
        &tbl,
        &[
            (1, "東京 駅 から 新宿 駅 まで 電車 で 移動 する"),
            (2, "京都 の 桜 は 四月 に 満開 を 迎える"),
            (3, "hello world from row 3"),
        ],
    )
    .await?;
    let index_result = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("whitespace".to_string())),
        )
        .execute()
        .await;
    match index_result {
        Ok(()) => finding(
            "axis_f_create_index_whitespace",
            "create_index FTS(whitespace) succeeded — no language_model_home involvement",
        ),
        Err(e) => {
            finding(
                "axis_f_create_index_whitespace",
                format!("create_index FTS(whitespace) FAILED: {e}"),
            );
            return Ok(());
        }
    }
    for (label, query) in &[
        ("axis_f_query_jp_tokyo", "東京"),
        ("axis_f_query_jp_kyoto", "京都"),
        ("axis_f_query_en_hello", "hello"),
        ("axis_f_query_jp_unsegmented", "東京駅"),
    ] {
        let stream_result = tbl
            .query()
            .full_text_search(FullTextSearchQuery::new((*query).to_string()))
            .limit(10)
            .execute()
            .await;
        match stream_result {
            Ok(stream) => {
                let batches: Vec<RecordBatch> = stream.try_collect().await?;
                let hits: usize = batches.iter().map(|b| b.num_rows()).sum();
                finding(label, format!("query {query:?} hits = {hits}"));
            }
            Err(e) => finding(label, format!("query {query:?} FAILED: {e}")),
        }
    }
    Ok(())
}

// ───────────────────────────── axis G — production path ───────────────────

/// The production design end-to-end: redirect `LANCE_LANGUAGE_MODEL_HOME`
/// to a tempdir, write the tiny `config.yml` (`embedded://ipadic`), and
/// call `create_index(FTS, "lindera/ipadic")`. Requires the
/// `lindera/embed-ipadic` cargo feature to be enabled on mdya
/// (= embedded IPADIC dictionary), otherwise the `embedded://ipadic`
/// scheme is not registered and `LinderaTokenizer::from_file` fails
/// with `"Invalid dictionary type: IPADIC"`.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_g_production_path_create_index_succeeds() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let home_tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(home_tmp.path());
    prepare_lindera_ipadic_home(&home_tmp)?;

    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_with_bodies(&tbl, &[(1, "hello world")]).await?;
    let attempt = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await;
    match attempt {
        Ok(()) => finding(
            "axis_g_production_path_create_index",
            "create_index FTS(lindera/ipadic) with env redirect + kind:ipadic config succeeded",
        ),
        Err(e) => finding(
            "axis_g_production_path_create_index",
            format!("create_index FAILED: {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis H — Japanese tokenisation ──────────────

/// With the production design (= env redirect + kind:ipadic config +
/// `lindera/ipadic` feature), does the embedded dictionary actually
/// segment Japanese well enough that `full_text_search` for an
/// unsegmented Japanese substring returns the expected rows? Comparison
/// with axis F (which misses the same query) demonstrates the
/// morphological advantage of the lindera/ipadic path.
#[tokio::test]
#[ignore = "verifies real Lance behaviour; run via `just lindera-ipadic-verify`"]
async fn axis_h_japanese_tokenization_via_embedded_ipadic() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let home_tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(home_tmp.path());
    prepare_lindera_ipadic_home(&home_tmp)?;

    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_with_bodies(
        &tbl,
        &[
            (1, "東京駅から新宿駅まで電車で移動する"),
            (2, "京都の桜は四月に満開を迎える"),
            (3, "hello world from row 3"),
        ],
    )
    .await?;
    let index_result = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await;
    if let Err(e) = index_result {
        finding("axis_h_create_index", format!("create_index FAILED: {e}"));
        return Ok(());
    }
    finding(
        "axis_h_create_index",
        "create_index FTS(lindera/ipadic) succeeded for Japanese corpus",
    );
    for (label, query) in &[
        ("axis_h_query_jp_tokyo", "東京"),
        ("axis_h_query_jp_kyoto", "京都"),
        ("axis_h_query_jp_unsegmented_tokyo_station", "東京駅"),
        ("axis_h_query_en_hello", "hello"),
    ] {
        let stream_result = tbl
            .query()
            .full_text_search(FullTextSearchQuery::new((*query).to_string()))
            .limit(10)
            .execute()
            .await;
        match stream_result {
            Ok(stream) => {
                let batches: Vec<RecordBatch> = stream.try_collect().await?;
                let hits: usize = batches.iter().map(|b| b.num_rows()).sum();
                finding(label, format!("query {query:?} hits = {hits}"));
            }
            Err(e) => finding(label, format!("query {query:?} FAILED: {e}")),
        }
    }
    Ok(())
}
