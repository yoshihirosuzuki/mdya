//! The networks of the models run on this PC: turning token ids into a hidden
//! state per token, on the CPU.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::{bert, modernbert};

use crate::types::BoxError;

/// Turns token ids into hidden states.
pub(super) trait Encoder: Send + Sync {
    /// `ids` and `mask` are `u32`, shaped (texts, tokens); the mask is 1 for a
    /// token and 0 for padding. Returns `f32`, shaped (texts, tokens, hidden).
    fn hidden_states(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor>;
}

/// The kind of network a model has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Architecture {
    ModernBert,
    Bert,
}

/// Loads the network from `config.json` and `model.safetensors` in `dir`.
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
    }
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
