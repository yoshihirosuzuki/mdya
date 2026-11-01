//! The networks of the models run on this PC: turning token ids into a hidden
//! state per token, on the CPU.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use candle_transformers::models::{bert, modernbert};

use crate::types::BoxError;

mod gemma3;

/// Turns token ids into hidden states.
pub(super) trait Encoder: Send + Sync {
    /// `ids` and `mask` are `u32`, shaped (texts, tokens); the mask is 1 for a
    /// token and 0 for padding. Returns `f32`, shaped (texts, tokens, hidden).
    fn hidden_states(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor>;

    /// Applies the layers that the model's pipeline puts after the mean of the
    /// hidden states, if it has any. `means` is `f32`, shaped (texts, hidden).
    fn project(&self, means: &Tensor) -> candle_core::Result<Tensor> {
        Ok(means.clone())
    }
}

/// The kind of network a model has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Architecture {
    ModernBert,
    Bert,
    /// Gemma 3 with attention both ways, followed after the mean by the two
    /// dense layers of embeddinggemma's pipeline.
    Gemma3Dense,
}

/// The files of the model that the network is loaded from, besides
/// `config.json` and `model.safetensors`.
pub(super) fn extra_files(architecture: Architecture) -> &'static [&'static str] {
    match architecture {
        Architecture::ModernBert | Architecture::Bert => &[],
        Architecture::Gemma3Dense => &DENSE_FILES,
    }
}

/// The weights of embeddinggemma's dense layers, in the order they run.
const DENSE_FILES: [&str; 2] = ["2_Dense/model.safetensors", "3_Dense/model.safetensors"];

/// Loads the network from `config.json`, `model.safetensors`, and the
/// `extra_files` in `dir`.
pub(super) fn load(architecture: Architecture, dir: &Path) -> Result<Box<dyn Encoder>, BoxError> {
    let config = std::fs::read(dir.join("config.json"))?;
    let tensors = candle_core::safetensors::load(dir.join("model.safetensors"), &Device::Cpu)?;
    match architecture {
        Architecture::ModernBert => {
            let config: modernbert::Config = serde_json::from_slice(&config)?;
            // candle's ModernBERT looks the weights up under `model.`, which
            // the names in the base model's file do not start with.
            let tensors: HashMap<String, Tensor> = tensors
                .into_iter()
                .map(|(name, tensor)| (format!("model.{name}"), tensor))
                .collect();
            let weights = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
            let model = modernbert::ModernBert::load(weights, &config)?;
            Ok(Box::new(ModernBertEncoder(model)))
        }
        Architecture::Bert => {
            let config: bert::Config = serde_json::from_slice(&config)?;
            let weights = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
            let model = bert::BertModel::load(weights, &config)?;
            Ok(Box::new(BertEncoder(model)))
        }
        Architecture::Gemma3Dense => {
            let config: gemma3::Config = serde_json::from_slice(&config)?;
            let weights = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
            let network = gemma3::Gemma3::load(weights, &config)?;
            let dense = DENSE_FILES
                .iter()
                .map(|file| dense_layer(&dir.join(file)))
                .collect::<Result<_, _>>()?;
            Ok(Box::new(Gemma3DenseEncoder { network, dense }))
        }
    }
}

/// A sentence-transformers dense layer without bias, whose weight is
/// `linear.weight` in `path`. Its shape gives the sizes of the layer.
fn dense_layer(path: &Path) -> Result<Linear, BoxError> {
    let tensors = candle_core::safetensors::load(path, &Device::Cpu)?;
    let weights = VarBuilder::from_tensors(tensors, DType::F32, &Device::Cpu);
    Ok(Linear::new(weights.get_unchecked("linear.weight")?, None))
}

struct ModernBertEncoder(modernbert::ModernBert);

impl Encoder for ModernBertEncoder {
    fn hidden_states(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
        self.0.forward(ids, mask)
    }
}

struct BertEncoder(bert::BertModel);

impl Encoder for BertEncoder {
    fn hidden_states(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
        // Every token is in the first (and only) segment.
        let token_type_ids = ids.zeros_like()?;
        self.0.forward(ids, &token_type_ids, Some(mask))
    }
}

struct Gemma3DenseEncoder {
    network: gemma3::Gemma3,
    dense: Vec<Linear>,
}

impl Encoder for Gemma3DenseEncoder {
    fn hidden_states(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
        self.network.forward(ids, mask)
    }

    fn project(&self, means: &Tensor) -> candle_core::Result<Tensor> {
        self.dense
            .iter()
            .try_fold(means.clone(), |x, layer| layer.forward(&x))
    }
}
