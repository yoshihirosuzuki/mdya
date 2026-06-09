//! Production embedding e2e (network required).
//!
//! Downloads `cl-nagoya/ruri-v3-30m` at its pinned revision into a temp cache,
//! constructs the real `RuriV3_30m` embedder, and exercises both `embed_queries`
//! and `embed_passages` end-to-end. Asserts the output dimension matches the
//! contract (256). Marked `#[ignore]` so `cargo test` and the
//! `just check` gate stay offline; run with `just embed-e2e` (or
//! `cargo test --test e2e_embed_real -- --ignored`).

use anyhow::Result;
use mdya::embedding::{Embedder, ModelCache, RuriV3_30m};
use tempfile::TempDir;

const EXPECTED_DIM: usize = 256;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network required: downloads cl-nagoya/ruri-v3-30m"]
async fn ruri_v3_30m_real_forward_dim_matches_adr() -> Result<()> {
    let cache_dir = TempDir::new()?;
    let cache = ModelCache::new(cache_dir.path())?;
    let embedder = RuriV3_30m::new(&cache).await?;

    assert_eq!(embedder.model_id(), "cl-nagoya/ruri-v3-30m");
    assert_eq!(embedder.dim(), EXPECTED_DIM);

    let queries = embedder.embed_queries(&["瑠璃色はどんな色?", "release checklist"])?;
    assert_eq!(queries.len(), 2);
    for row in &queries {
        assert_eq!(row.len(), EXPECTED_DIM);
    }

    let passages = embedder.embed_passages(&[
        "瑠璃色は紫みを帯びた濃い青である。",
        "Always run `just check` before opening a PR.",
    ])?;
    assert_eq!(passages.len(), 2);
    for row in &passages {
        assert_eq!(row.len(), EXPECTED_DIM);
    }

    Ok(())
}
