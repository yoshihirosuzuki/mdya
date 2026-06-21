//! EmbeddingGemma pipeline: tokenize → Gemma3 encoder → mean pool → two dense
//! projections → L2 normalize, producing one 768-dim vector per input.
//!
//! The dense head shape and pooling mode are fixed to EmbeddingGemma's
//! `modules.json` (mean pooling; Dense 768→3072 then 3072→768, both linear with
//! no bias and identity activation; final L2 normalize).

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use tokenizers::{PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer};

use super::model::{Config, Gemma3Model};

/// EmbeddingGemma's two dense layers (`2_Dense` then `3_Dense`).
const DENSE_IN: usize = 768;
const DENSE_HIDDEN: usize = 3072;
const DENSE_OUT: usize = 768;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("parse config.json: {0}")]
    Config(#[from] serde_json::Error),

    #[error("config.json has no integer pad_token_id")]
    MissingPadToken,

    #[error("pad token id {0} is not in the tokenizer vocabulary")]
    PadTokenNotInVocab(u32),

    #[error("tokenizer: {0}")]
    Tokenizer(String),

    #[error("candle: {0}")]
    Candle(#[from] candle_core::Error),
}

/// On-disk files for one EmbeddingGemma checkpoint. The caller (embedding host)
/// owns downloading, caching, and gated-repo authentication.
pub struct ModelFiles<'a> {
    /// `config.json` (Gemma3 text config).
    pub config: &'a Path,
    /// `tokenizer.json`.
    pub tokenizer: &'a Path,
    /// `model.safetensors` (the transformer; keys at the root).
    pub weights: &'a Path,
    /// `2_Dense/model.safetensors` (768 → 3072).
    pub dense2: &'a Path,
    /// `3_Dense/model.safetensors` (3072 → 768).
    pub dense3: &'a Path,
}

/// A loaded EmbeddingGemma model ready to embed plain text.
pub struct EmbeddingGemma {
    model: Gemma3Model,
    tokenizer: Tokenizer,
    dense2: Linear,
    dense3: Linear,
    device: Device,
}

impl EmbeddingGemma {
    /// Load the model from on-disk files into memory (CPU, f32). No network
    /// access — `files` must already exist.
    pub fn load(files: &ModelFiles) -> Result<Self, Error> {
        let device = Device::Cpu;
        let dtype = DType::F32;

        let config_bytes = read(files.config)?;
        let cfg: Config = serde_json::from_slice(&config_bytes)?;
        let cfg_value: serde_json::Value = serde_json::from_slice(&config_bytes)?;
        let pad_id = u32::try_from(
            cfg_value
                .get("pad_token_id")
                .and_then(serde_json::Value::as_u64)
                .ok_or(Error::MissingPadToken)?,
        )
        .map_err(|_| Error::MissingPadToken)?;

        let mut tokenizer = Tokenizer::from_file(files.tokenizer)
            .map_err(|e| Error::Tokenizer(format!("load: {e}")))?;
        let pad_token = tokenizer
            .id_to_token(pad_id)
            .ok_or(Error::PadTokenNotInVocab(pad_id))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            direction: PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id,
            pad_type_id: 0,
            pad_token,
        }));

        let weights = candle_core::safetensors::load(files.weights, &device)?;
        let model = Gemma3Model::load(
            VarBuilder::from_tensors(weights, dtype, &device),
            &cfg,
            &device,
        )?;

        let dense2 = load_dense(files.dense2, DENSE_IN, DENSE_HIDDEN, dtype, &device)?;
        let dense3 = load_dense(files.dense3, DENSE_HIDDEN, DENSE_OUT, dtype, &device)?;

        Ok(Self {
            model,
            tokenizer,
            dense2,
            dense3,
            device,
        })
    }

    /// Embed plain texts. The caller is responsible for prepending any retrieval
    /// task prompt (EmbeddingGemma uses different query vs document prompts).
    /// Returns one L2-normalized 768-dim vector per input, in order.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<String> = texts.iter().map(|t| (*t).to_string()).collect();
        let encodings = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| Error::Tokenizer(format!("encode_batch: {e}")))?;

        let batch = encodings.len();
        let seq = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq);
        for enc in &encodings {
            // `encode_batch` pads to the longest row; treat a ragged row as an
            // internal invariant breach with a clear message rather than letting
            // `Tensor::from_vec` fail later with an opaque shape error.
            if enc.get_ids().len() != seq {
                return Err(Error::Tokenizer(format!(
                    "encode_batch produced ragged rows: max_len={seq}, row={}",
                    enc.get_ids().len()
                )));
            }
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }
        let input_ids = Tensor::from_vec(ids, (batch, seq), &self.device)?;
        let attention_mask = Tensor::from_vec(mask, (batch, seq), &self.device)?;

        let hidden = self.model.forward(&input_ids, &attention_mask)?;
        let pooled = mean_pool(&hidden, &attention_mask)?;
        let projected = self.dense3.forward(&self.dense2.forward(&pooled)?)?;
        let normalized = l2_normalize(&projected)?;
        Ok(normalized.to_vec2::<f32>()?)
    }
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Load a `sentence_transformers.models.Dense` layer (no bias, identity
/// activation) from its `model.safetensors`, whose single weight is keyed
/// `linear.weight`.
fn load_dense(
    path: &Path,
    in_features: usize,
    out_features: usize,
    dtype: DType,
    device: &Device,
) -> Result<Linear, Error> {
    let tensors = candle_core::safetensors::load(path, device)?;
    let vb = VarBuilder::from_tensors(tensors, dtype, device);
    Ok(candle_nn::linear_no_bias(
        in_features,
        out_features,
        vb.pp("linear"),
    )?)
}

/// Mean-pool `hidden` (`[b, seq, h]`) over the sequence, weighting by the
/// padding `mask` (`[b, seq]`, 1 = real, 0 = pad).
fn mean_pool(hidden: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
    let mask = mask.to_dtype(hidden.dtype())?;
    let mask_expanded = mask.unsqueeze(2)?.broadcast_as(hidden.shape())?;
    let summed = hidden.mul(&mask_expanded)?.sum(1)?;
    let counts = mask.sum_keepdim(1)?.clamp(1.0f32, f32::INFINITY)?;
    summed.broadcast_div(&counts)
}

/// L2-normalize each row of `x` (`[b, h]`).
fn l2_normalize(x: &Tensor) -> candle_core::Result<Tensor> {
    let norm = x
        .sqr()?
        .sum_keepdim(1)?
        .sqrt()?
        .clamp(1e-12f32, f32::INFINITY)?;
    x.broadcast_div(&norm)
}
