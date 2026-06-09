//! Generates `tests/fixtures/tiny_modernbert/` (`model.safetensors`,
//! `config.json`, `tokenizer.json`) from a randomly initialized minimal
//! ModernBERT graph. Run with `just gen-fixtures` (or `cargo run -p
//! xtask-generate-tiny-modernbert`); commit the resulting files so the
//! modernbert smoke does not require network access.
//!
//! The emitted `config.json` matches the field layout that
//! `candle_transformers::models::modernbert::Config` deserializes from. The
//! weight names match the prefix path that `ModernBert::load` traverses
//! (`model.embeddings.*`, `model.layers.{i}.*`, `model.final_norm.weight`).
//! The tokenizer reuses the same hand-built WordPiece vocab as the tiny BERT
//! fixture; ModernBERT's forward only consumes token IDs and an attention
//! mask, so the tokenizer kind does not matter for the wire-up smoke.

use std::path::{Path, PathBuf};

use ahash::AHashMap;
use anyhow::Result;
use candle_core::{DType, Device};
use candle_nn::{VarBuilder, VarMap};
use serde::Serialize;
use tokenizers::Tokenizer;
use tokenizers::models::wordpiece::WordPiece;

const VOCAB_SIZE: usize = 6;
const HIDDEN_SIZE: usize = 8;
const NUM_HIDDEN_LAYERS: usize = 2;
const NUM_ATTENTION_HEADS: usize = 2;
const INTERMEDIATE_SIZE: usize = 16;
const MAX_POSITION_EMBEDDINGS: usize = 16;
const LAYER_NORM_EPS: f64 = 1e-12;
const PAD_TOKEN_ID: u32 = 0;
const GLOBAL_ATTN_EVERY_N_LAYERS: usize = 3;
const GLOBAL_ROPE_THETA: f64 = 160_000.0;
const LOCAL_ATTENTION: usize = 8;
const LOCAL_ROPE_THETA: f64 = 10_000.0;

const VOCAB: &[&str] = &["[PAD]", "[UNK]", "[CLS]", "[SEP]", "hello", "world"];

#[derive(Serialize)]
struct ModernBertConfigJson {
    vocab_size: usize,
    hidden_size: usize,
    num_hidden_layers: usize,
    num_attention_heads: usize,
    intermediate_size: usize,
    max_position_embeddings: usize,
    layer_norm_eps: f64,
    pad_token_id: u32,
    global_attn_every_n_layers: usize,
    global_rope_theta: f64,
    local_attention: usize,
    local_rope_theta: f64,
}

fn fixture_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above xtask manifest")
        .join("tests")
        .join("fixtures")
        .join("tiny_modernbert")
}

fn write_config(out_dir: &Path) -> Result<()> {
    let cfg = ModernBertConfigJson {
        vocab_size: VOCAB_SIZE,
        hidden_size: HIDDEN_SIZE,
        num_hidden_layers: NUM_HIDDEN_LAYERS,
        num_attention_heads: NUM_ATTENTION_HEADS,
        intermediate_size: INTERMEDIATE_SIZE,
        max_position_embeddings: MAX_POSITION_EMBEDDINGS,
        layer_norm_eps: LAYER_NORM_EPS,
        pad_token_id: PAD_TOKEN_ID,
        global_attn_every_n_layers: GLOBAL_ATTN_EVERY_N_LAYERS,
        global_rope_theta: GLOBAL_ROPE_THETA,
        local_attention: LOCAL_ATTENTION,
        local_rope_theta: LOCAL_ROPE_THETA,
    };
    std::fs::write(
        out_dir.join("config.json"),
        serde_json::to_string_pretty(&cfg)?,
    )?;
    Ok(())
}

fn write_tokenizer(out_dir: &Path) -> Result<()> {
    let vocab: AHashMap<String, u32> = VOCAB
        .iter()
        .enumerate()
        .map(|(i, t)| ((*t).to_string(), i as u32))
        .collect();
    let model = WordPiece::builder()
        .vocab(vocab)
        .unk_token("[UNK]".to_string())
        .build()
        .map_err(|e| anyhow::anyhow!("build WordPiece: {e}"))?;
    let tokenizer = Tokenizer::new(model);
    tokenizer
        .save(out_dir.join("tokenizer.json"), true)
        .map_err(|e| anyhow::anyhow!("save tokenizer.json: {e}"))?;
    Ok(())
}

fn write_weights(out_dir: &Path) -> Result<()> {
    // The weight key layout below mirrors `candle_transformers::models::
    // modernbert::ModernBert::load` for the pinned candle version
    // (`=0.10.1` in the workspace Cargo.toml). If candle bumps and renames or
    // restructures any path under `model.*`, re-run `just gen-fixtures` after
    // updating the pinned version so the committed fixture stays loadable.
    let device = Device::Cpu;
    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

    // Embeddings.
    let emb = vb.pp("model.embeddings");
    let _ = emb.get((VOCAB_SIZE, HIDDEN_SIZE), "tok_embeddings.weight")?;
    let _ = emb.get(HIDDEN_SIZE, "norm.weight")?;

    // Encoder layers.
    for layer_idx in 0..NUM_HIDDEN_LAYERS {
        let layer = vb.pp(format!("model.layers.{layer_idx}"));
        let attn = layer.pp("attn");
        // linear_no_bias(in=H, out=H*3) -> weight shape (out=H*3, in=H)
        let _ = attn.get((HIDDEN_SIZE * 3, HIDDEN_SIZE), "Wqkv.weight")?;
        // linear_no_bias(in=H, out=H)
        let _ = attn.get((HIDDEN_SIZE, HIDDEN_SIZE), "Wo.weight")?;

        let mlp = layer.pp("mlp");
        // linear_no_bias(in=H, out=I*2) -> (I*2, H)
        let _ = mlp.get((INTERMEDIATE_SIZE * 2, HIDDEN_SIZE), "Wi.weight")?;
        // linear_no_bias(in=I, out=H) -> (H, I)
        let _ = mlp.get((HIDDEN_SIZE, INTERMEDIATE_SIZE), "Wo.weight")?;

        // attn_norm is optional in ModernBertLayer::load (.ok()); writing it
        // for every layer is safe and exercises the Some-branch in load.
        let _ = layer.get(HIDDEN_SIZE, "attn_norm.weight")?;
        let _ = layer.get(HIDDEN_SIZE, "mlp_norm.weight")?;
    }

    let _ = vb.pp("model").get(HIDDEN_SIZE, "final_norm.weight")?;

    varmap.save(out_dir.join("model.safetensors"))?;
    Ok(())
}

fn main() -> Result<()> {
    let out_dir = fixture_dir();
    std::fs::create_dir_all(&out_dir)?;
    write_config(&out_dir)?;
    write_tokenizer(&out_dir)?;
    write_weights(&out_dir)?;
    println!("wrote tiny_modernbert fixture to {}", out_dir.display());
    Ok(())
}
