//! Gemma3 bidirectional encoder for `google/embeddinggemma-300m`.
//!
//! Produces the transformer's last hidden state `[batch, seq_len, hidden_size]`.
//! Pooling, the dense projection heads, and L2 normalization are the caller's
//! responsibility; this module deliberately stops at the final RMSNorm output.
//!
//! Adapted from Hugging Face's text-embeddings-inference `gemma3.rs`
//! (Apache-2.0; see the root `NOTICE`). The TEI flash-attention / cuBLASLt /
//! batching / tracing infrastructure is dropped in favour of a plain candle CPU
//! path. Rotary embedding is provided by [`super::rotary`].

use candle_core::{D, DType, Device, Result, Tensor};
use candle_nn::{Embedding, Linear, Module, VarBuilder};
use serde::Deserialize;

/// Gemma3 text-config subset needed to build the encoder. Field names mirror the
/// model's `config.json` (the `text_config` block of EmbeddingGemma). Fields
/// that may be absent from a given checkpoint carry `#[serde(default)]`, matching
/// TEI's handling.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub num_key_value_heads: usize,
    pub head_dim: usize,
    pub vocab_size: usize,
    pub max_position_embeddings: usize,
    pub rms_norm_eps: f64,
    pub query_pre_attn_scalar: f64,
    /// RoPE base for full-attention layers.
    pub rope_theta: f32,
    /// RoPE base for sliding-window layers.
    pub rope_local_base_freq: f32,
    pub sliding_window: usize,
    /// Every `sliding_window_pattern`-th layer (1-based) is full attention; the
    /// rest are sliding-window. EmbeddingGemma's `config.json` exposes this under
    /// the `_sliding_window_pattern` key (matching TEI's serde rename).
    #[serde(rename = "_sliding_window_pattern")]
    pub sliding_window_pattern: usize,
    #[serde(default)]
    pub attention_bias: bool,
    pub use_bidirectional_attention: bool,
}

/// Gemma3 RMSNorm. Differs from the canonical RMSNorm in the post-normalization
/// scale: Gemma multiplies by `(weight + 1.0)` rather than `weight`.
#[derive(Debug)]
struct Gemma3RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl Gemma3RmsNorm {
    fn load(vb: VarBuilder, size: usize, eps: f64) -> Result<Self> {
        let weight = vb.get(size, "weight")?;
        Ok(Self { weight, eps })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let in_dtype = x.dtype();
        // Accumulate the variance in f32 to avoid precision loss in low-precision
        // dtypes, matching TEI.
        let x = x.to_dtype(DType::F32)?;
        let hidden = x.dim(D::Minus1)?;
        let variance = (x.sqr()?.sum_keepdim(D::Minus1)? / hidden as f64)?;
        let normed = x.broadcast_div(&(variance + self.eps)?.sqrt()?)?;
        let normed = normed.to_dtype(in_dtype)?;
        // Gemma convention: scale by (weight + 1.0).
        normed.broadcast_mul(&(&self.weight + 1.0)?)
    }
}

/// Attention variant: full attention every `sliding_window_pattern`-th layer,
/// sliding-window otherwise. The variant selects both the RoPE base and the mask
/// shape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttentionKind {
    Full,
    Sliding,
}

struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: Gemma3RmsNorm,
    k_norm: Gemma3RmsNorm,
    num_heads: usize,
    num_kv_heads: usize,
    head_dim: usize,
    scaling: f64,
}

impl Attention {
    fn load(vb: VarBuilder, cfg: &Config) -> Result<Self> {
        let num_heads = cfg.num_attention_heads;
        let num_kv_heads = cfg.num_key_value_heads;
        let head_dim = cfg.head_dim;
        let hidden = cfg.hidden_size;

        // `attention_bias` is false for EmbeddingGemma, so the projections carry
        // no bias.
        let (q_proj, k_proj, v_proj, o_proj) = if cfg.attention_bias {
            (
                candle_nn::linear(hidden, num_heads * head_dim, vb.pp("q_proj"))?,
                candle_nn::linear(hidden, num_kv_heads * head_dim, vb.pp("k_proj"))?,
                candle_nn::linear(hidden, num_kv_heads * head_dim, vb.pp("v_proj"))?,
                candle_nn::linear(num_heads * head_dim, hidden, vb.pp("o_proj"))?,
            )
        } else {
            (
                candle_nn::linear_no_bias(hidden, num_heads * head_dim, vb.pp("q_proj"))?,
                candle_nn::linear_no_bias(hidden, num_kv_heads * head_dim, vb.pp("k_proj"))?,
                candle_nn::linear_no_bias(hidden, num_kv_heads * head_dim, vb.pp("v_proj"))?,
                candle_nn::linear_no_bias(num_heads * head_dim, hidden, vb.pp("o_proj"))?,
            )
        };

        let q_norm = Gemma3RmsNorm::load(vb.pp("q_norm"), head_dim, cfg.rms_norm_eps)?;
        let k_norm = Gemma3RmsNorm::load(vb.pp("k_norm"), head_dim, cfg.rms_norm_eps)?;

        // Gemma3 scales by 1/sqrt(query_pre_attn_scalar), not 1/sqrt(head_dim).
        let scaling = 1.0 / cfg.query_pre_attn_scalar.sqrt();

        Ok(Self {
            q_proj,
            k_proj,
            v_proj,
            o_proj,
            q_norm,
            k_norm,
            num_heads,
            num_kv_heads,
            head_dim,
            scaling,
        })
    }

    /// `hidden_states`: `[b, seq, hidden]`. `attn_mask`: additive mask broadcast
    /// over `[b, num_heads, seq, seq]` (0 for attended positions, a large
    /// negative value (≈ -∞) for masked). `cos`/`sin`: rotary tables for this
    /// layer's RoPE base.
    fn forward(
        &self,
        hidden_states: &Tensor,
        attn_mask: &Tensor,
        cos: &Tensor,
        sin: &Tensor,
    ) -> Result<Tensor> {
        let (b, seq, _) = hidden_states.dims3()?;

        let q = self.q_proj.forward(hidden_states)?;
        let k = self.k_proj.forward(hidden_states)?;
        let v = self.v_proj.forward(hidden_states)?;

        // Reshape to [b, seq, heads, head_dim] for per-head q/k normalization
        // (Gemma3 normalizes over head_dim before rotary).
        let q = q.reshape((b, seq, self.num_heads, self.head_dim))?;
        let k = k.reshape((b, seq, self.num_kv_heads, self.head_dim))?;
        let v = v.reshape((b, seq, self.num_kv_heads, self.head_dim))?;

        let q = self.q_norm.forward(&q)?;
        let k = self.k_norm.forward(&k)?;

        // -> [b, heads, seq, head_dim]
        let q = q.transpose(1, 2)?.contiguous()?;
        let k = k.transpose(1, 2)?.contiguous()?;
        let v = v.transpose(1, 2)?.contiguous()?;

        let q = super::rotary::apply(&q, cos, sin, self.head_dim)?;
        let k = super::rotary::apply(&k, cos, sin, self.head_dim)?;

        // GQA: repeat the kv heads to match the number of query heads.
        let k = repeat_kv(&k, self.num_heads / self.num_kv_heads)?;
        let v = repeat_kv(&v, self.num_heads / self.num_kv_heads)?;

        let attn_weights = (q.matmul(&k.transpose(2, 3)?.contiguous()?)? * self.scaling)?;
        let attn_weights = attn_weights.broadcast_add(attn_mask)?;
        let attn_weights = candle_nn::ops::softmax_last_dim(&attn_weights)?;
        let context = attn_weights.matmul(&v)?;

        // -> [b, seq, heads * head_dim]
        let context = context
            .transpose(1, 2)?
            .reshape((b, seq, self.num_heads * self.head_dim))?;
        self.o_proj.forward(&context)
    }
}

/// Expand kv heads by `n_rep` along the head axis (GQA). Returns the input
/// unchanged when `n_rep == 1`.
fn repeat_kv(x: &Tensor, n_rep: usize) -> Result<Tensor> {
    if n_rep == 1 {
        return Ok(x.clone());
    }
    let (b, h, s, d) = x.dims4()?;
    x.unsqueeze(2)?
        .expand((b, h, n_rep, s, d))?
        .reshape((b, h * n_rep, s, d))
}

struct Mlp {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
}

impl Mlp {
    fn load(vb: VarBuilder, cfg: &Config) -> Result<Self> {
        let h = cfg.hidden_size;
        let i = cfg.intermediate_size;
        Ok(Self {
            gate_proj: candle_nn::linear_no_bias(h, i, vb.pp("gate_proj"))?,
            up_proj: candle_nn::linear_no_bias(h, i, vb.pp("up_proj"))?,
            down_proj: candle_nn::linear_no_bias(i, h, vb.pp("down_proj"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        // gelu_pytorch_tanh: candle's `gelu` is the tanh approximation.
        let gate = self.gate_proj.forward(x)?.gelu()?;
        let up = self.up_proj.forward(x)?;
        self.down_proj.forward(&(gate * up)?)
    }
}

struct DecoderLayer {
    input_layernorm: Gemma3RmsNorm,
    self_attn: Attention,
    post_attention_layernorm: Gemma3RmsNorm,
    pre_feedforward_layernorm: Gemma3RmsNorm,
    mlp: Mlp,
    post_feedforward_layernorm: Gemma3RmsNorm,
    kind: AttentionKind,
}

impl DecoderLayer {
    fn load(vb: VarBuilder, cfg: &Config, kind: AttentionKind) -> Result<Self> {
        let eps = cfg.rms_norm_eps;
        let h = cfg.hidden_size;
        Ok(Self {
            input_layernorm: Gemma3RmsNorm::load(vb.pp("input_layernorm"), h, eps)?,
            self_attn: Attention::load(vb.pp("self_attn"), cfg)?,
            post_attention_layernorm: Gemma3RmsNorm::load(
                vb.pp("post_attention_layernorm"),
                h,
                eps,
            )?,
            pre_feedforward_layernorm: Gemma3RmsNorm::load(
                vb.pp("pre_feedforward_layernorm"),
                h,
                eps,
            )?,
            mlp: Mlp::load(vb.pp("mlp"), cfg)?,
            post_feedforward_layernorm: Gemma3RmsNorm::load(
                vb.pp("post_feedforward_layernorm"),
                h,
                eps,
            )?,
            kind,
        })
    }

    fn forward(
        &self,
        x: &Tensor,
        attn_mask: &Tensor,
        cos: &Tensor,
        sin: &Tensor,
    ) -> Result<Tensor> {
        // Gemma3 sandwich norms:
        //   h   = x + post_attention_layernorm(attn(input_layernorm(x)))
        //   out = h + post_feedforward_layernorm(mlp(pre_feedforward_layernorm(h)))
        let residual = x;
        let hidden = self.input_layernorm.forward(x)?;
        let hidden = self.self_attn.forward(&hidden, attn_mask, cos, sin)?;
        let hidden = self.post_attention_layernorm.forward(&hidden)?;
        let hidden = (residual + hidden)?;

        let residual = &hidden;
        let mlp_in = self.pre_feedforward_layernorm.forward(&hidden)?;
        let mlp_out = self.mlp.forward(&mlp_in)?;
        let mlp_out = self.post_feedforward_layernorm.forward(&mlp_out)?;
        residual + mlp_out
    }
}

/// Gemma3 bidirectional encoder.
pub struct Gemma3Model {
    embed_tokens: Embedding,
    embed_scale: f64,
    layers: Vec<DecoderLayer>,
    norm: Gemma3RmsNorm,
    head_dim: usize,
    rope_theta_full: f32,
    rope_theta_local: f32,
    sliding_window: usize,
    use_bidirectional_attention: bool,
    device: Device,
    dtype: DType,
}

impl Gemma3Model {
    /// Load the encoder. EmbeddingGemma's `model.safetensors` stores the
    /// transformer at the root (no `model.` prefix), so `vb` is rooted directly
    /// at the parameters: the keys consumed are `embed_tokens.weight`,
    /// `layers.{i}.…`, and `norm.weight`.
    pub fn load(vb: VarBuilder, cfg: &Config, device: &Device) -> Result<Self> {
        let dtype = vb.dtype();

        let embed_tokens =
            candle_nn::embedding(cfg.vocab_size, cfg.hidden_size, vb.pp("embed_tokens"))?;
        // Gemma3 scales token embeddings by sqrt(hidden_size).
        let embed_scale = (cfg.hidden_size as f64).sqrt();

        let layers = (0..cfg.num_hidden_layers)
            .map(|i| {
                // TEI: layer i is full attention when (i + 1) % pattern == 0,
                // sliding-window otherwise.
                let kind = if (i + 1) % cfg.sliding_window_pattern == 0 {
                    AttentionKind::Full
                } else {
                    AttentionKind::Sliding
                };
                DecoderLayer::load(vb.pp(format!("layers.{i}")), cfg, kind)
            })
            .collect::<Result<Vec<_>>>()?;

        let norm = Gemma3RmsNorm::load(vb.pp("norm"), cfg.hidden_size, cfg.rms_norm_eps)?;

        Ok(Self {
            embed_tokens,
            embed_scale,
            layers,
            norm,
            head_dim: cfg.head_dim,
            rope_theta_full: cfg.rope_theta,
            rope_theta_local: cfg.rope_local_base_freq,
            sliding_window: cfg.sliding_window,
            use_bidirectional_attention: cfg.use_bidirectional_attention,
            device: device.clone(),
            dtype,
        })
    }

    /// Encode a batch. `input_ids`: `[b, seq]` token ids. `attention_mask`:
    /// `[b, seq]` padding mask (1 = real token, 0 = padding). Returns the final
    /// hidden state `[b, seq, hidden_size]`.
    pub fn forward(&self, input_ids: &Tensor, attention_mask: &Tensor) -> Result<Tensor> {
        let (b, seq) = input_ids.dims2()?;

        let mut hidden = (self.embed_tokens.forward(input_ids)? * self.embed_scale)?;

        // Sequential positions 0..seq (encoder, no KV-cache offset).
        let inv_full =
            super::rotary::inverse_frequencies(self.head_dim, self.rope_theta_full, &self.device)?;
        let (cos_full, sin_full) = super::rotary::cos_sin(seq, &inv_full, self.dtype)?;
        let inv_local =
            super::rotary::inverse_frequencies(self.head_dim, self.rope_theta_local, &self.device)?;
        let (cos_local, sin_local) = super::rotary::cos_sin(seq, &inv_local, self.dtype)?;

        let padding_bias = self.padding_bias(attention_mask, b, seq)?;
        let full_mask = self.build_mask(&padding_bias, b, seq, AttentionKind::Full)?;
        let sliding_mask = self.build_mask(&padding_bias, b, seq, AttentionKind::Sliding)?;

        for layer in &self.layers {
            let (mask, cos, sin) = match layer.kind {
                AttentionKind::Full => (&full_mask, &cos_full, &sin_full),
                AttentionKind::Sliding => (&sliding_mask, &cos_local, &sin_local),
            };
            hidden = layer.forward(&hidden, mask, cos, sin)?;
        }

        self.norm.forward(&hidden)
    }

    /// Additive padding bias of shape `[b, 1, 1, seq]`: 0 where the caller's mask
    /// is 1, a large negative value (≈ -∞) where it is 0. Broadcasts over heads
    /// and query positions.
    fn padding_bias(&self, attention_mask: &Tensor, b: usize, seq: usize) -> Result<Tensor> {
        let mask = attention_mask
            .to_dtype(self.dtype)?
            .reshape((b, 1, 1, seq))?;
        let zeros = Tensor::zeros((b, 1, 1, seq), self.dtype, &self.device)?;
        let neg = Tensor::full(min_value(self.dtype), (b, 1, 1, seq), &self.device)?
            .to_dtype(self.dtype)?;
        // mask == 1 -> 0, mask == 0 -> -inf
        let keep = mask.ge(0.5)?;
        keep.where_cond(&zeros, &neg)
    }

    /// Combine the per-position attention pattern (port of TEI's
    /// `create_attention_mask`) with the caller's padding bias into an additive
    /// mask broadcast to `[b, 1, seq, seq]`.
    fn build_mask(
        &self,
        padding_bias: &Tensor,
        b: usize,
        seq: usize,
        kind: AttentionKind,
    ) -> Result<Tensor> {
        let sliding_window = match kind {
            AttentionKind::Sliding => Some(self.sliding_window),
            AttentionKind::Full => None,
        };

        // Port of TEI `create_attention_mask`: a u8 allow/deny grid.
        let pattern: Vec<u8> = (0..seq)
            .flat_map(|i| {
                (0..seq).map(move |j| {
                    let allowed = if self.use_bidirectional_attention {
                        if let Some(window_size) = sliding_window {
                            // Bidirectional sliding window: a token attends to any
                            // other whose absolute distance is within half the
                            // sliding window size.
                            let half_window = window_size / 2;
                            i.abs_diff(j) <= half_window
                        } else {
                            true
                        }
                    } else if let Some(window_size) = sliding_window {
                        j <= i && i - j < window_size
                    } else {
                        j <= i
                    };
                    allowed as u8
                })
            })
            .collect();

        let allow = Tensor::from_slice(&pattern, (seq, seq), &self.device)?;
        let allow = allow.expand((b, 1, seq, seq))?;
        let zeros = Tensor::zeros((b, 1, seq, seq), self.dtype, &self.device)?;
        let neg = Tensor::full(min_value(self.dtype), (b, 1, seq, seq), &self.device)?
            .to_dtype(self.dtype)?;
        let pattern_bias = allow.where_cond(&zeros, &neg)?;

        // Combine with the caller's padding bias (broadcast [b,1,1,seq]).
        pattern_bias.broadcast_add(padding_bias)
    }
}

/// Large-negative sentinel used as the masked-out additive bias.
fn min_value(dtype: DType) -> f32 {
    match dtype {
        DType::F32 => f32::MIN,
        // f16 minimum representable value.
        _ => -65504.0,
    }
}
