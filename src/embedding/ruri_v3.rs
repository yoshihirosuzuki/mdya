//! `RuriV3_30m` — default embedder.
//!
//! Backed by `cl-nagoya/ruri-v3-30m` (ModernBERT-Ja-30m fine-tuned for Japanese
//! retrieval, 256-dim, Apache 2.0). Applies the retrieval prefixes
//! `検索クエリ: ` / `検索文書: ` internally so callers pass plain text.
//!
//! Mean-pools the last hidden state with the attention mask, then L2-normalizes
//! the row so cosine similarity is well-defined (LanceDB index uses
//! `DistanceType::Cosine`).

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::modernbert::{Config, ModernBert};
use tokenizers::{PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer};

use super::{EmbedError, Embedder, ModelCache, RetrievalPrefixes, find_on_device_preset};

pub const RURI_V3_30M_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";

/// Pinned commit hash of `cl-nagoya/ruri-v3-30m` on Hugging Face. Captured
/// from `huggingface.co/api/models/cl-nagoya/ruri-v3-30m` (main) at the time
/// this embedder was added (2026-05-25). Bumped only via a deliberate
/// version change — the revision is code-hard-coded, not surfaced in
/// `config.yml`.
pub const RURI_V3_30M_REVISION: &str = "24899e5de370b56d179604a007c0d727bf144504";

/// Output vector dimension. Re-exported as `RURI_V3_30M_DIM` from
/// the `embedding` module so callers (e.g. `mdya init` building the chunks
/// table schema) can pin the LanceDB `FixedSizeList` without instantiating
/// the embedder.
pub const HIDDEN_SIZE: usize = 256;

pub struct RuriV3_30m {
    model: ModernBert,
    tokenizer: Tokenizer,
    device: Device,
    /// Retrieval prefixes sourced from this model's entry in the embedding
    /// preset registry, applied in `embed_queries` / `embed_passages`.
    prefixes: RetrievalPrefixes,
}

impl RuriV3_30m {
    /// Fetch the three required files via `cache`, then load the ModernBERT
    /// graph and tokenizer into memory. Network access happens at most on the
    /// first call per filesystem cache; subsequent calls are pure disk hits.
    pub async fn new(cache: &ModelCache) -> Result<Self, EmbedError> {
        let config_path = cache
            .fetch_model_file(RURI_V3_30M_MODEL_ID, RURI_V3_30M_REVISION, "config.json")
            .await?;
        let tokenizer_path = cache
            .fetch_model_file(RURI_V3_30M_MODEL_ID, RURI_V3_30M_REVISION, "tokenizer.json")
            .await?;
        let weights_path = cache
            .fetch_model_file(
                RURI_V3_30M_MODEL_ID,
                RURI_V3_30M_REVISION,
                "model.safetensors",
            )
            .await?;

        let config: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
        // Defense against silent `config.json` / `RURI_V3_30M_REVISION` drift:
        // catch it the moment we load the file instead of way deeper in the
        // forward pass.
        if config.hidden_size != HIDDEN_SIZE {
            return Err(EmbedError::Forward(format!(
                "config.hidden_size={} disagrees with compiled HIDDEN_SIZE={HIDDEN_SIZE} \
                 (revision {RURI_V3_30M_REVISION} may have shifted)",
                config.hidden_size
            )));
        }
        // Truncation is intentionally not configured. ruri-v3-30m supports
        // 8192 tokens via ModernBERT RoPE; Markdown inputs rarely exceed
        // that, and overflow surfaces as a candle error at forward time. If
        // long-context inputs become routine, add `TruncationParams`.
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedError::Tokenizer(format!("load: {e}")))?;
        // ruri-v3 ships its tokenizer.json without a baked-in padding policy;
        // enable BatchLongest right-side padding so `encode_batch` returns
        // uniform-length rows that we can stack into a single tensor.
        let pad_id = config.pad_token_id;
        let pad_token = tokenizer
            .id_to_token(pad_id)
            .ok_or_else(|| EmbedError::Tokenizer(format!("pad token id {pad_id} not in vocab")))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            direction: PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id,
            pad_type_id: 0,
            pad_token,
        }));

        let device = Device::Cpu;
        // ruri-v3-30m is published via SentenceTransformers, so its safetensors
        // keys are root-rooted (`embeddings.tok_embeddings.weight`, etc.). The
        // upstream `candle_transformers::models::modernbert::ModernBert::load`
        // expects the canonical answerdotai/ModernBERT layout (`model.*`), so
        // we re-key every tensor with a `model.` prefix before handing it to
        // `VarBuilder::from_tensors`.
        let raw = candle_core::safetensors::load(&weights_path, &device)?;
        let renamed: std::collections::HashMap<String, Tensor> = raw
            .into_iter()
            .map(|(k, v)| (format!("model.{k}"), v))
            .collect();
        let vb = VarBuilder::from_tensors(renamed, DType::F32, &device);
        let model = ModernBert::load(vb, &config)?;

        // Retrieval prefixes live in the preset registry so every embedder
        // applies them through the same seam; ruri is a compile-time entry, so
        // a missing lookup is a programmer error, not a runtime condition.
        let prefixes = find_on_device_preset(RURI_V3_30M_MODEL_ID)
            .expect("ruri-v3-30m must be registered in ON_DEVICE_PRESETS")
            .prefixes;

        Ok(Self {
            model,
            tokenizer,
            device,
            prefixes,
        })
    }

    fn embed_internal(&self, prefixed: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Empty input must not reach candle: `Tensor::from_vec` of an empty
        // batch produces zero-sized tensors that the ModernBERT forward pass
        // is not built to consume.
        if prefixed.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(prefixed.to_vec(), true)
            .map_err(|e| EmbedError::Tokenizer(format!("encode_batch: {e}")))?;

        let batch_size = encodings.len();
        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);

        let mut ids: Vec<u32> = Vec::with_capacity(batch_size * max_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch_size * max_len);
        for enc in &encodings {
            if enc.get_ids().len() != max_len {
                // tokenizers::encode_batch pads to the longest row by default;
                // we treat a violation here as an internal invariant breach
                // rather than re-padding manually.
                return Err(EmbedError::Tokenizer(format!(
                    "encode_batch produced ragged rows: max_len={max_len}, row={}",
                    enc.get_ids().len()
                )));
            }
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }

        let input_ids = Tensor::from_vec(ids, (batch_size, max_len), &self.device)?;
        let attention_mask = Tensor::from_vec(mask, (batch_size, max_len), &self.device)?;

        // candle errors propagate as `EmbedError::Candle` via `#[from]`; only
        // the post-processing helpers (`mean_pool`, `l2_normalize`, `to_vec2`)
        // and shape invariants wear the `Forward` variant.
        let hidden = self.model.forward(&input_ids, &attention_mask)?;
        let pooled = mean_pool(&hidden, &attention_mask)?;
        let normalized = l2_normalize(&pooled)?;

        let vectors = normalized
            .to_vec2::<f32>()
            .map_err(|e| EmbedError::Forward(format!("to_vec2: {e}")))?;
        if vectors.iter().any(|v| v.len() != HIDDEN_SIZE) {
            return Err(EmbedError::Forward(format!(
                "expected dim={HIDDEN_SIZE}, got ragged output"
            )));
        }
        Ok(vectors)
    }
}

impl Embedder for RuriV3_30m {
    fn model_id(&self) -> &str {
        RURI_V3_30M_MODEL_ID
    }

    fn dim(&self) -> usize {
        HIDDEN_SIZE
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefixed: Vec<String> = texts.iter().map(|t| self.prefixes.apply_query(t)).collect();
        self.embed_internal(&prefixed)
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| self.prefixes.apply_passage(t))
            .collect();
        self.embed_internal(&prefixed)
    }
}

fn mean_pool(hidden: &Tensor, attention_mask: &Tensor) -> Result<Tensor, candle_core::Error> {
    // hidden : [batch, seq_len, hidden_size]
    // mask   : [batch, seq_len] (1 = real, 0 = pad)
    let mask_f = attention_mask
        .unsqueeze(2)?
        .to_dtype(hidden.dtype())?
        .broadcast_as(hidden.shape())?;
    let masked = hidden.mul(&mask_f)?;
    let summed = masked.sum(1)?;
    let counts = attention_mask
        .sum_keepdim(1)?
        .to_dtype(hidden.dtype())?
        .clamp(1.0_f32, f32::INFINITY)?;
    summed.broadcast_div(&counts)
}

fn l2_normalize(rows: &Tensor) -> Result<Tensor, candle_core::Error> {
    // L2-normalize along the last dim (per-row).
    let sq = rows.sqr()?;
    let sum = sq.sum_keepdim(1)?;
    let norm = sum.sqrt()?.clamp(1e-12_f32, f32::INFINITY)?;
    rows.broadcast_div(&norm)
}
