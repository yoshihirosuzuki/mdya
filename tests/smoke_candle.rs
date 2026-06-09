//! candle smoke: loads the committed `tests/fixtures/tiny_bert/` (generated
//! by `xtask-generate-tiny-bert`), tokenizes a short input, runs a BERT
//! forward pass, and asserts the embedding tensor has the expected hidden
//! size. Verifies the candle wire-up across all 3 supported platforms with
//! no network access (fixture lives in repo).

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tiny_bert")
}

#[test]
fn bert_forward_returns_hidden_size_embedding() -> Result<()> {
    let dir = fixture_dir();
    let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow!("load tokenizer: {e}"))?;

    let device = Device::Cpu;
    let weights_path = dir.join("model.safetensors");
    let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)? };
    let model = BertModel::load(vb, &config)?;

    let encoding = tokenizer
        .encode("hello world", true)
        .map_err(|e| anyhow!("encode: {e}"))?;
    let ids: Vec<u32> = encoding.get_ids().to_vec();
    let type_ids: Vec<u32> = encoding.get_type_ids().to_vec();
    let token_ids = Tensor::new(ids.as_slice(), &device)?.unsqueeze(0)?;
    let token_type_ids = Tensor::new(type_ids.as_slice(), &device)?.unsqueeze(0)?;

    let output = model.forward(&token_ids, &token_type_ids, None)?;
    let dims = output.dims();
    assert_eq!(
        dims.last().copied(),
        Some(config.hidden_size),
        "expected hidden_size={} at last dim, got {dims:?}",
        config.hidden_size
    );
    Ok(())
}
