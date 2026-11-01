//! The Gemma 3 network that google/embeddinggemma-300m runs, with attention
//! both ways: token ids in, a hidden state per token out.
//!
//! Adapted from the candle backend of Hugging Face's
//! text-embeddings-inference (`backends/candle/src/models/gemma3.rs` and
//! `backends/candle/src/layers/rotary.rs`, Apache-2.0; see `NOTICE`), and
//! checked against `transformers/models/gemma3` of transformers 4.57.0.

use candle_core::{D, DType, Device, Tensor};
use candle_nn::{Embedding, Linear, Module, VarBuilder};
use serde::Deserialize;

use crate::types::BoxError;

/// The part of the model's `config.json` that the network needs.
#[derive(Debug, Deserialize)]
pub(super) struct Config {
    hidden_size: usize,
    intermediate_size: usize,
    num_attention_heads: usize,
    num_key_value_heads: usize,
    head_dim: usize,
    vocab_size: usize,
    rms_norm_eps: f64,
    query_pre_attn_scalar: f64,
    /// The rotary base of the full-attention layers.
    rope_theta: f32,
    /// The rotary base of the sliding-window layers.
    rope_local_base_freq: f32,
    sliding_window: usize,
    layer_types: Vec<LayerType>,
    #[serde(default)]
    attention_bias: bool,
    use_bidirectional_attention: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LayerType {
    SlidingAttention,
    FullAttention,
}

/// The network, loaded.
pub(super) struct Gemma3 {
    embed_tokens: Embedding,
    embed_scale: f64,
    layers: Vec<Layer>,
    norm: RmsNorm,
    head_dim: usize,
    rope_theta: f32,
    rope_local_base_freq: f32,
    sliding_window: usize,
}

impl Gemma3 {
    /// Loads the network from `weights`, whose names start at the network
    /// (`embed_tokens.weight`, `layers.0.…`, `norm.weight`).
    pub(super) fn load(weights: VarBuilder, config: &Config) -> Result<Self, BoxError> {
        if !config.use_bidirectional_attention {
            return Err(Box::new(CausalAttention));
        }
        let layers = config
            .layer_types
            .iter()
            .enumerate()
            .map(|(i, kind)| Layer::load(weights.pp(format!("layers.{i}")), config, *kind))
            .collect::<candle_core::Result<Vec<_>>>()?;
        Ok(Self {
            embed_tokens: candle_nn::embedding(
                config.vocab_size,
                config.hidden_size,
                weights.pp("embed_tokens"),
            )?,
            embed_scale: (config.hidden_size as f64).sqrt(),
            layers,
            norm: RmsNorm::load(weights.pp("norm"), config.hidden_size, config.rms_norm_eps)?,
            head_dim: config.head_dim,
            rope_theta: config.rope_theta,
            rope_local_base_freq: config.rope_local_base_freq,
            sliding_window: config.sliding_window,
        })
    }

    /// `ids` and `mask` are `u32`, shaped (texts, tokens); the mask is 1 for a
    /// token and 0 for padding. Returns `f32`, shaped (texts, tokens, hidden).
    pub(super) fn forward(&self, ids: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
        let tokens = ids.dim(1)?;
        let full = Context {
            bias: attention_bias(mask, None)?,
            rotary: Rotary::new(tokens, self.head_dim, self.rope_theta)?,
        };
        let sliding = Context {
            bias: attention_bias(mask, Some(self.sliding_window))?,
            rotary: Rotary::new(tokens, self.head_dim, self.rope_local_base_freq)?,
        };
        let mut hidden = (self.embed_tokens.forward(ids)? * self.embed_scale)?;
        for layer in &self.layers {
            let context = match layer.kind {
                LayerType::FullAttention => &full,
                LayerType::SlidingAttention => &sliding,
            };
            hidden = layer.forward(&hidden, context)?;
        }
        self.norm.forward(&hidden)
    }
}

/// What the layers of one kind share in a call: the attention bias and the
/// rotary tables.
struct Context {
    bias: Tensor,
    rotary: Rotary,
}

/// Whether a token may attend to another in a sliding-window layer.
/// embeddinggemma's `config.json` gives `sliding_window: 512`, and with
/// attention both ways, a token attends to the tokens at most half of it
/// away, on either side: transformers 4.57.0 sets the window to
/// `sliding_window // 2 + 1` (`configuration_gemma3.py`, lines 228-229) and
/// allows `abs(q_idx - kv_idx) < sliding_window` (`modeling_gemma3.py`, line
/// 450); text-embeddings-inference (`gemma3.rs`) and llama.cpp
/// (`llama-hparams.h`, `LLAMA_SWA_TYPE_SYMMETRIC`) allow distances up to half
/// the window.
fn within_window(query: usize, key: usize, sliding_window: usize) -> bool {
    query.abs_diff(key) <= sliding_window / 2
}

/// The bias added to the attention scores, shaped (texts, 1, tokens, tokens):
/// 0 where a token may attend to another, and `f32::MIN` where it may not,
/// because the other is padding or, in a sliding-window layer, too far away.
/// The bias for a masked score is finite, not minus infinity: a padding token
/// beyond the window of every real token has all its scores masked, a row of
/// minus infinity would make the softmax produce NaN, and the next layer would
/// spread NaN to the real tokens.
fn attention_bias(mask: &Tensor, sliding_window: Option<usize>) -> candle_core::Result<Tensor> {
    let (texts, tokens) = mask.dims2()?;
    let pattern: Vec<u8> = (0..tokens)
        .flat_map(|query| {
            (0..tokens).map(move |key| {
                u8::from(sliding_window.is_none_or(|window| within_window(query, key, window)))
            })
        })
        .collect();
    let pattern = Tensor::from_vec(pattern, (1, 1, tokens, tokens), &Device::Cpu)?;
    let keys = mask.to_dtype(DType::U8)?.reshape((texts, 1, 1, tokens))?;
    let allowed = pattern.broadcast_mul(&keys)?;
    let shape = (texts, 1, tokens, tokens);
    let attend = Tensor::zeros(shape, DType::F32, &Device::Cpu)?;
    let masked = Tensor::full(f32::MIN, shape, &Device::Cpu)?;
    allowed.where_cond(&attend, &masked)
}

/// The rotary position tables for one base, shaped (tokens, head size).
struct Rotary {
    cos: Tensor,
    sin: Tensor,
}

impl Rotary {
    fn new(tokens: usize, head_dim: usize, base: f32) -> candle_core::Result<Self> {
        let inverse: Vec<f32> = (0..head_dim)
            .step_by(2)
            .map(|i| 1.0 / base.powf(i as f32 / head_dim as f32))
            .collect();
        let half = inverse.len();
        let inverse = Tensor::from_vec(inverse, (1, half), &Device::Cpu)?;
        let positions = Tensor::arange(0_u32, tokens as u32, &Device::Cpu)?
            .to_dtype(DType::F32)?
            .reshape((tokens, 1))?;
        let angles = positions.matmul(&inverse)?;
        let angles = Tensor::cat(&[&angles, &angles], 1)?;
        Ok(Self {
            cos: angles.cos()?,
            sin: angles.sin()?,
        })
    }

    /// Rotates `x`, shaped (texts, heads, tokens, head size), by position,
    /// rotating the two halves of each head into each other.
    fn apply(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let half = x.dim(D::Minus1)? / 2;
        let first = x.narrow(D::Minus1, 0, half)?;
        let second = x.narrow(D::Minus1, half, half)?;
        let rotated = Tensor::cat(&[&second.neg()?, &first], D::Minus1)?;
        x.broadcast_mul(&self.cos)? + rotated.broadcast_mul(&self.sin)?
    }
}

/// Gemma's RMS normalization, which scales by the weight plus 1.
struct RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl RmsNorm {
    fn load(weights: VarBuilder, size: usize, eps: f64) -> candle_core::Result<Self> {
        Ok(Self {
            weight: weights.get(size, "weight")?,
            eps,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let size = x.dim(D::Minus1)? as f64;
        let mean_square = (x.sqr()?.sum_keepdim(D::Minus1)? / size)?;
        let normalized = x.broadcast_div(&(mean_square + self.eps)?.sqrt()?)?;
        normalized.broadcast_mul(&(&self.weight + 1.0)?)
    }
}

struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
    scale: f64,
}

impl Attention {
    fn load(weights: VarBuilder, config: &Config) -> candle_core::Result<Self> {
        let hidden = config.hidden_size;
        let heads = config.num_attention_heads;
        let kv_heads = config.num_key_value_heads;
        let head_dim = config.head_dim;
        let bias = config.attention_bias;
        let linear =
            |input, output, name| candle_nn::linear_b(input, output, bias, weights.pp(name));
        Ok(Self {
            q_proj: linear(hidden, heads * head_dim, "q_proj")?,
            k_proj: linear(hidden, kv_heads * head_dim, "k_proj")?,
            v_proj: linear(hidden, kv_heads * head_dim, "v_proj")?,
            o_proj: linear(heads * head_dim, hidden, "o_proj")?,
            q_norm: RmsNorm::load(weights.pp("q_norm"), head_dim, config.rms_norm_eps)?,
            k_norm: RmsNorm::load(weights.pp("k_norm"), head_dim, config.rms_norm_eps)?,
            heads,
            kv_heads,
            head_dim,
            scale: 1.0 / config.query_pre_attn_scalar.sqrt(),
        })
    }

    fn forward(&self, x: &Tensor, context: &Context) -> candle_core::Result<Tensor> {
        let (texts, tokens, _) = x.dims3()?;
        let q = self.heads_of(&self.q_proj, &self.q_norm, x, self.heads)?;
        let k = self.heads_of(&self.k_proj, &self.k_norm, x, self.kv_heads)?;
        let q = context.rotary.apply(&q)?;
        let k = context.rotary.apply(&k)?;
        let v = self
            .v_proj
            .forward(x)?
            .reshape((texts, tokens, self.kv_heads, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let groups = self.heads / self.kv_heads;
        let k = repeat_heads(&k, groups)?;
        let v = repeat_heads(&v, groups)?;
        let scores = (q.matmul(&k.transpose(2, 3)?.contiguous()?)? * self.scale)?;
        let weights = candle_nn::ops::softmax_last_dim(&scores.broadcast_add(&context.bias)?)?;
        let attended = weights.matmul(&v)?.transpose(1, 2)?.reshape((
            texts,
            tokens,
            self.heads * self.head_dim,
        ))?;
        self.o_proj.forward(&attended)
    }

    /// The queries or keys of `x`, normalized per head before rotation,
    /// shaped (texts, heads, tokens, head size).
    fn heads_of(
        &self,
        projection: &Linear,
        norm: &RmsNorm,
        x: &Tensor,
        heads: usize,
    ) -> candle_core::Result<Tensor> {
        let (texts, tokens, _) = x.dims3()?;
        let projected = projection
            .forward(x)?
            .reshape((texts, tokens, heads, self.head_dim))?;
        norm.forward(&projected)?.transpose(1, 2)?.contiguous()
    }
}

/// `x`, shaped (texts, heads, tokens, head size), with each head repeated
/// `groups` times, so that a few key and value heads serve all query heads.
fn repeat_heads(x: &Tensor, groups: usize) -> candle_core::Result<Tensor> {
    let (texts, heads, tokens, head_dim) = x.dims4()?;
    x.unsqueeze(2)?
        .expand((texts, heads, groups, tokens, head_dim))?
        .reshape((texts, heads * groups, tokens, head_dim))
}

struct Mlp {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
}

impl Mlp {
    fn load(weights: VarBuilder, config: &Config) -> candle_core::Result<Self> {
        let hidden = config.hidden_size;
        let intermediate = config.intermediate_size;
        Ok(Self {
            gate_proj: candle_nn::linear_no_bias(hidden, intermediate, weights.pp("gate_proj"))?,
            up_proj: candle_nn::linear_no_bias(hidden, intermediate, weights.pp("up_proj"))?,
            down_proj: candle_nn::linear_no_bias(intermediate, hidden, weights.pp("down_proj"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        // The configuration's `gelu_pytorch_tanh` is the tanh approximation,
        // which is candle's `gelu`.
        let gate = self.gate_proj.forward(x)?.gelu()?;
        self.down_proj.forward(&(gate * self.up_proj.forward(x)?)?)
    }
}

/// A layer, with Gemma's normalization before and after both the attention
/// and the feed-forward part.
struct Layer {
    input_layernorm: RmsNorm,
    self_attn: Attention,
    post_attention_layernorm: RmsNorm,
    pre_feedforward_layernorm: RmsNorm,
    mlp: Mlp,
    post_feedforward_layernorm: RmsNorm,
    kind: LayerType,
}

impl Layer {
    fn load(weights: VarBuilder, config: &Config, kind: LayerType) -> candle_core::Result<Self> {
        let norm = |name| RmsNorm::load(weights.pp(name), config.hidden_size, config.rms_norm_eps);
        Ok(Self {
            input_layernorm: norm("input_layernorm")?,
            self_attn: Attention::load(weights.pp("self_attn"), config)?,
            post_attention_layernorm: norm("post_attention_layernorm")?,
            pre_feedforward_layernorm: norm("pre_feedforward_layernorm")?,
            mlp: Mlp::load(weights.pp("mlp"), config)?,
            post_feedforward_layernorm: norm("post_feedforward_layernorm")?,
            kind,
        })
    }

    fn forward(&self, x: &Tensor, context: &Context) -> candle_core::Result<Tensor> {
        let attended = self
            .self_attn
            .forward(&self.input_layernorm.forward(x)?, context)?;
        let x = (x + self.post_attention_layernorm.forward(&attended)?)?;
        let fed = self
            .mlp
            .forward(&self.pre_feedforward_layernorm.forward(&x)?)?;
        &x + self.post_feedforward_layernorm.forward(&fed)?
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the Gemma 3 configuration asks for causal attention, which this network does not run")]
struct CausalAttention;

#[cfg(test)]
mod tests {
    use candle_nn::VarMap;

    use super::*;

    /// A small configuration: two layers, one of each kind, with a sliding
    /// window of 4, so that tokens more than 2 apart do not see each other in
    /// the sliding-window layer.
    fn config(bidirectional: bool) -> Config {
        serde_json::from_value(serde_json::json!({
            "hidden_size": 8,
            "intermediate_size": 16,
            "num_attention_heads": 2,
            "num_key_value_heads": 1,
            "head_dim": 4,
            "vocab_size": 32,
            "rms_norm_eps": 1e-6,
            "query_pre_attn_scalar": 4,
            "rope_theta": 1_000_000.0,
            "rope_local_base_freq": 10_000.0,
            "sliding_window": 4,
            "layer_types": ["sliding_attention", "full_attention"],
            "attention_bias": false,
            "use_bidirectional_attention": bidirectional,
        }))
        .unwrap()
    }

    /// A network with random weights.
    fn network() -> Gemma3 {
        let weights = VarMap::new();
        Gemma3::load(
            VarBuilder::from_varmap(&weights, DType::F32, &Device::Cpu),
            &config(true),
        )
        .unwrap()
    }

    fn ids_and_mask(rows: &[&[u32]]) -> (Tensor, Tensor) {
        let width = rows.iter().map(|row| row.len()).max().unwrap();
        let mut ids = Vec::new();
        let mut mask = Vec::new();
        for row in rows {
            ids.extend(
                row.iter()
                    .copied()
                    .chain(std::iter::repeat_n(0, width - row.len())),
            );
            mask.extend(
                std::iter::repeat_n(1_u32, row.len())
                    .chain(std::iter::repeat_n(0, width - row.len())),
            );
        }
        let shape = (rows.len(), width);
        (
            Tensor::from_vec(ids, shape, &Device::Cpu).unwrap(),
            Tensor::from_vec(mask, shape, &Device::Cpu).unwrap(),
        )
    }

    #[test]
    fn attends_up_to_half_the_sliding_window_away_on_either_side() {
        // embeddinggemma's window of 512 reaches 256 tokens away, not 257.
        assert!(within_window(300, 300 - 256, 512));
        assert!(within_window(300, 300 + 256, 512));
        assert!(!within_window(300, 300 - 257, 512));
        assert!(!within_window(300, 300 + 257, 512));
    }

    fn bias(mask: &Tensor, sliding_window: Option<usize>) -> Vec<Vec<Vec<f32>>> {
        attention_bias(mask, sliding_window)
            .unwrap()
            .squeeze(1)
            .unwrap()
            .to_vec3::<f32>()
            .unwrap()
    }

    #[test]
    fn masks_padding_and_far_tokens_with_one_finite_bias_each() {
        // The second text has one token, and three of padding.
        let (_, mask) = ids_and_mask(&[&[1, 1, 1, 1], &[1]]);
        let m = f32::MIN;

        let full = bias(&mask, None);
        let sliding = bias(&mask, Some(2));

        assert_eq!(full[0], vec![vec![0.0; 4]; 4]);
        assert_eq!(full[1], vec![vec![0.0, m, m, m]; 4]);
        // A window of 2 reaches one token away.
        assert_eq!(
            sliding[0],
            [
                vec![0.0, 0.0, m, m],
                vec![0.0, 0.0, 0.0, m],
                vec![m, 0.0, 0.0, 0.0],
                vec![m, m, 0.0, 0.0],
            ]
        );
        // The last padding token is beyond the window of the real token, so
        // all its scores are masked, each by one `f32::MIN`.
        assert_eq!(sliding[1][3], [m, m, m, m]);
    }

    #[test]
    fn gives_a_text_the_same_hidden_states_alone_and_padded_beside_a_longer_one() {
        let network = network();
        let short: &[u32] = &[2, 5, 7];
        // Long enough that the padding at its end is beyond the sliding window
        // of every token of the short text.
        let long: Vec<u32> = (1..=20).collect();

        let (ids, mask) = ids_and_mask(&[short]);
        let alone = network.forward(&ids, &mask).unwrap();
        let (ids, mask) = ids_and_mask(&[&long, short]);
        let beside = network.forward(&ids, &mask).unwrap();

        let alone = alone.get(0).unwrap().to_vec2::<f32>().unwrap();
        let beside = beside.get(1).unwrap().to_vec2::<f32>().unwrap();
        assert!(beside.iter().flatten().all(|value| value.is_finite()));
        for (a, b) in alone.iter().flatten().zip(beside.iter().flatten()) {
            assert!((a - b).abs() < 1e-4, "alone {a}, beside a longer text {b}");
        }
    }

    #[test]
    fn leaves_the_first_position_unrotated() {
        let rotary = Rotary::new(1, 4, 10_000.0).unwrap();
        let x = Tensor::from_vec(vec![1_f32, 2.0, 3.0, 4.0], (1, 1, 1, 4), &Device::Cpu).unwrap();

        let rotated = rotary.apply(&x).unwrap();

        assert_eq!(
            rotated.flatten_all().unwrap().to_vec1::<f32>().unwrap(),
            [1.0, 2.0, 3.0, 4.0]
        );
    }

    #[test]
    fn normalizes_and_scales_by_the_weight_plus_one() {
        let norm = RmsNorm {
            weight: Tensor::from_vec(vec![0_f32, 1.0], 2, &Device::Cpu).unwrap(),
            eps: 0.0,
        };
        // The root mean square of [3, 4] is sqrt(12.5).
        let x = Tensor::from_vec(vec![3_f32, 4.0], (1, 2), &Device::Cpu).unwrap();

        let normalized = norm.forward(&x).unwrap().to_vec2::<f32>().unwrap();

        let rms = 12.5_f32.sqrt();
        assert!((normalized[0][0] - 3.0 / rms).abs() < 1e-6);
        assert!((normalized[0][1] - 2.0 * 4.0 / rms).abs() < 1e-6);
    }

    #[test]
    fn refuses_a_configuration_with_causal_attention() {
        let weights = VarMap::new();

        let error = Gemma3::load(
            VarBuilder::from_varmap(&weights, DType::F32, &Device::Cpu),
            &config(false),
        )
        .err()
        .unwrap();

        assert_eq!(
            error.to_string(),
            "the Gemma 3 configuration asks for causal attention, which this network does not run"
        );
    }
}
