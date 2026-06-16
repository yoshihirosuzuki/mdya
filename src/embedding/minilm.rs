//! `MiniLm` — `sentence-transformers/all-MiniLM-L6-v2` embedder.
//!
//! A small English BERT sentence-transformer (6-layer, 384-dim, Apache 2.0,
//! ~22 MB). Uses no retrieval prefix (queries and passages are embedded as
//! plain text), so the registry pairs it with empty prefixes.
//!
//! Mean-pools the last hidden state with the attention mask, then L2-normalizes
//! the row so cosine similarity is well-defined (LanceDB index uses
//! `DistanceType::Cosine`).

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer};

use super::pooling::{l2_normalize, mean_pool};
use super::{EmbedError, Embedder, ModelCache, RetrievalPrefixes, find_on_device_preset};

pub const MINILM_L6_V2_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Pinned commit hash of `sentence-transformers/all-MiniLM-L6-v2` on Hugging
/// Face. Captured from `huggingface.co/api/models/...` (main) when this
/// embedder was added (2026-06-16). Bumped only via a deliberate version
/// change — the revision is code-hard-coded, not surfaced in `config.yml`.
pub const MINILM_L6_V2_REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

/// Output vector dimension. Re-exported as `MINILM_L6_V2_DIM` from the
/// `embedding` module so the on-device preset registry and the schema-dim
/// resolver can pin the LanceDB `FixedSizeList` without instantiating the
/// embedder.
pub const HIDDEN_SIZE: usize = 384;

pub struct MiniLm {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    /// Retrieval prefixes sourced from this model's entry in the embedding
    /// preset registry (empty for MiniLM), applied in `embed_queries` /
    /// `embed_passages`.
    prefixes: RetrievalPrefixes,
}

impl MiniLm {
    /// Fetch the three required files via `cache`, then load the BERT graph and
    /// tokenizer into memory. Network access happens at most on the first call
    /// per filesystem cache; subsequent calls are pure disk hits.
    pub async fn new(cache: &ModelCache) -> Result<Self, EmbedError> {
        let config_path = cache
            .fetch_model_file(MINILM_L6_V2_MODEL_ID, MINILM_L6_V2_REVISION, "config.json")
            .await?;
        let tokenizer_path = cache
            .fetch_model_file(MINILM_L6_V2_MODEL_ID, MINILM_L6_V2_REVISION, "tokenizer.json")
            .await?;
        let weights_path = cache
            .fetch_model_file(
                MINILM_L6_V2_MODEL_ID,
                MINILM_L6_V2_REVISION,
                "model.safetensors",
            )
            .await?;

        let config: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
        // Defense against silent `config.json` / `MINILM_L6_V2_REVISION` drift:
        // catch a dim mismatch at load instead of deep in the forward pass.
        if config.hidden_size != HIDDEN_SIZE {
            return Err(EmbedError::Forward(format!(
                "config.hidden_size={} disagrees with compiled HIDDEN_SIZE={HIDDEN_SIZE} \
                 (revision {MINILM_L6_V2_REVISION} may have shifted)",
                config.hidden_size
            )));
        }

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedError::Tokenizer(format!("load: {e}")))?;
        // Enable BatchLongest right-side padding so `encode_batch` returns
        // uniform-length rows that stack into a single tensor.
        let pad_id = u32::try_from(config.pad_token_id)
            .map_err(|_| EmbedError::Tokenizer(format!("pad_token_id {} too large", config.pad_token_id)))?;
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
        // all-MiniLM-L6-v2's `config.json` declares `model_type: "bert"`, so
        // `BertModel::load` resolves both the root (`embeddings.*`) and the
        // SentenceTransformers (`bert.embeddings.*`) key layouts via its
        // built-in fallback — no manual re-keying is needed here.
        let raw = candle_core::safetensors::load(&weights_path, &device)?;
        let vb = VarBuilder::from_tensors(raw, DType::F32, &device);
        let model = BertModel::load(vb, &config)?;

        let prefixes = find_on_device_preset(MINILM_L6_V2_MODEL_ID)
            .expect("all-MiniLM-L6-v2 must be registered in ON_DEVICE_PRESETS")
            .prefixes;

        Ok(Self {
            model,
            tokenizer,
            device,
            prefixes,
        })
    }

    fn embed_internal(&self, prefixed: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Empty input must not reach candle: zero-sized tensors are not a
        // shape the BERT forward pass is built to consume.
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
        // Single-sentence inputs are all segment 0; BERT still requires the
        // token_type_ids tensor, unlike ModernBERT.
        let token_type_ids = Tensor::zeros((batch_size, max_len), DType::U32, &self.device)?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
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

impl Embedder for MiniLm {
    fn model_id(&self) -> &str {
        MINILM_L6_V2_MODEL_ID
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
