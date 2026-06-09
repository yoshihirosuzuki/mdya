//! Generates `tests/fixtures/tiny_bert/` (`model.safetensors`, `config.json`,
//! `tokenizer.json`) from a randomly initialized minimal BERT graph. Run once
//! with `just gen-fixtures` (or `cargo run -p xtask-generate-tiny-bert`);
//! commit the resulting files so CI does not need network access for the
//! candle smoke.
//!
//! Field names in the emitted `config.json` mirror the layout that
//! `candle_transformers::models::bert::Config` deserializes from (this xtask
//! does not depend on candle-transformers itself; the smoke test on the
//! consumer side does). The tokenizer uses a tiny hand-built WordPiece vocab
//! (`[PAD]`, `[UNK]`, `[CLS]`, `[SEP]`, `hello`, `world`). License is the
//! project license (MIT OR Apache-2.0) since everything is generated locally.

use std::path::{Path, PathBuf};

use ahash::AHashMap;
use anyhow::Result;
use candle_core::{DType, Device};
use candle_nn::{VarBuilder, VarMap};
use serde::Serialize;
use tokenizers::Tokenizer;
use tokenizers::models::wordpiece::WordPiece;

const HIDDEN_SIZE: usize = 8;
const NUM_HIDDEN_LAYERS: usize = 1;
const NUM_ATTENTION_HEADS: usize = 2;
const INTERMEDIATE_SIZE: usize = 16;
const MAX_POSITION_EMBEDDINGS: usize = 16;
const TYPE_VOCAB_SIZE: usize = 2;
const VOCAB: &[&str] = &["[PAD]", "[UNK]", "[CLS]", "[SEP]", "hello", "world"];

#[derive(Serialize)]
struct BertConfigJson {
    model_type: &'static str,
    vocab_size: usize,
    hidden_size: usize,
    num_hidden_layers: usize,
    num_attention_heads: usize,
    intermediate_size: usize,
    max_position_embeddings: usize,
    type_vocab_size: usize,
    hidden_act: &'static str,
    hidden_dropout_prob: f64,
    attention_probs_dropout_prob: f64,
    initializer_range: f64,
    layer_norm_eps: f64,
    pad_token_id: usize,
    classifier_dropout: Option<f64>,
    position_embedding_type: &'static str,
}

fn fixture_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above xtask manifest")
        .join("tests")
        .join("fixtures")
        .join("tiny_bert")
}

fn write_config(out_dir: &Path) -> Result<()> {
    let cfg = BertConfigJson {
        model_type: "bert",
        vocab_size: VOCAB.len(),
        hidden_size: HIDDEN_SIZE,
        num_hidden_layers: NUM_HIDDEN_LAYERS,
        num_attention_heads: NUM_ATTENTION_HEADS,
        intermediate_size: INTERMEDIATE_SIZE,
        max_position_embeddings: MAX_POSITION_EMBEDDINGS,
        type_vocab_size: TYPE_VOCAB_SIZE,
        hidden_act: "gelu",
        hidden_dropout_prob: 0.0,
        attention_probs_dropout_prob: 0.0,
        initializer_range: 0.02,
        layer_norm_eps: 1e-12,
        pad_token_id: 0,
        classifier_dropout: None,
        position_embedding_type: "absolute",
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
    let device = Device::Cpu;
    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

    // Embeddings.
    let embeddings = vb.pp("embeddings");
    let _ = embeddings.get((VOCAB.len(), HIDDEN_SIZE), "word_embeddings.weight")?;
    let _ = embeddings.get(
        (MAX_POSITION_EMBEDDINGS, HIDDEN_SIZE),
        "position_embeddings.weight",
    )?;
    let _ = embeddings.get(
        (TYPE_VOCAB_SIZE, HIDDEN_SIZE),
        "token_type_embeddings.weight",
    )?;
    let _ = embeddings.get(HIDDEN_SIZE, "LayerNorm.weight")?;
    let _ = embeddings.get(HIDDEN_SIZE, "LayerNorm.bias")?;

    // Encoder layer(s).
    for layer_idx in 0..NUM_HIDDEN_LAYERS {
        let layer = vb.pp(format!("encoder.layer.{layer_idx}"));
        let attn = layer.pp("attention");
        for proj in ["query", "key", "value"] {
            let p = attn.pp(format!("self.{proj}"));
            let _ = p.get((HIDDEN_SIZE, HIDDEN_SIZE), "weight")?;
            let _ = p.get(HIDDEN_SIZE, "bias")?;
        }
        let attn_out = attn.pp("output");
        let _ = attn_out.get((HIDDEN_SIZE, HIDDEN_SIZE), "dense.weight")?;
        let _ = attn_out.get(HIDDEN_SIZE, "dense.bias")?;
        let _ = attn_out.get(HIDDEN_SIZE, "LayerNorm.weight")?;
        let _ = attn_out.get(HIDDEN_SIZE, "LayerNorm.bias")?;

        let intermediate = layer.pp("intermediate");
        let _ = intermediate.get((INTERMEDIATE_SIZE, HIDDEN_SIZE), "dense.weight")?;
        let _ = intermediate.get(INTERMEDIATE_SIZE, "dense.bias")?;

        let output = layer.pp("output");
        let _ = output.get((HIDDEN_SIZE, INTERMEDIATE_SIZE), "dense.weight")?;
        let _ = output.get(HIDDEN_SIZE, "dense.bias")?;
        let _ = output.get(HIDDEN_SIZE, "LayerNorm.weight")?;
        let _ = output.get(HIDDEN_SIZE, "LayerNorm.bias")?;
    }

    // Pooler (optional but commonly expected).
    let pooler = vb.pp("pooler");
    let _ = pooler.get((HIDDEN_SIZE, HIDDEN_SIZE), "dense.weight")?;
    let _ = pooler.get(HIDDEN_SIZE, "dense.bias")?;

    varmap.save(out_dir.join("model.safetensors"))?;
    Ok(())
}

fn main() -> Result<()> {
    let out_dir = fixture_dir();
    std::fs::create_dir_all(&out_dir)?;
    write_config(&out_dir)?;
    write_tokenizer(&out_dir)?;
    write_weights(&out_dir)?;
    println!("wrote tiny_bert fixture to {}", out_dir.display());
    Ok(())
}
