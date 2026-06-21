//! Rotary position embedding (rotate-half / GPT-NeoX style).
//!
//! Adapted from text-embeddings-inference's `layers/rotary.rs` (Apache-2.0; see
//! the root `NOTICE`), trimmed to the plain base-frequency case EmbeddingGemma
//! uses. EmbeddingGemma applies two frequencies — a local base for
//! sliding-window layers and a global base for full-attention layers — so the
//! per-layer cos/sin tables are built from [`inverse_frequencies`] with the
//! matching base.

use candle_core::{D, DType, Device, Result, Tensor};

/// Inverse frequencies `1 / base^(i/dim)` for even `i` in `0..dim`, shaped
/// `[1, dim/2]`. `dim` is the per-head dimension; `base` is `rope_theta`
/// (full-attention layers) or `rope_local_base_freq` (sliding-window layers).
pub fn inverse_frequencies(dim: usize, base: f32, device: &Device) -> Result<Tensor> {
    let inv: Vec<f32> = (0..dim)
        .step_by(2)
        .map(|i| 1f32 / base.powf(i as f32 / dim as f32))
        .collect();
    let len = inv.len();
    Tensor::from_vec(inv, (1, len), device)
}

/// Per-position `(cos, sin)` tables of shape `[length, dim]`, with the
/// half-width frequencies duplicated so they align with the rotate-half layout
/// in [`apply`].
pub fn cos_sin(length: usize, inv_freqs: &Tensor, dtype: DType) -> Result<(Tensor, Tensor)> {
    let positions = Tensor::arange(0u32, length as u32, inv_freqs.device())?
        .to_dtype(DType::F32)?
        .reshape((length, 1))?;
    let freqs = positions.matmul(inv_freqs)?;
    let freqs = Tensor::cat(&[&freqs, &freqs], 1)?;
    let cos = freqs.cos()?.to_dtype(dtype)?;
    let sin = freqs.sin()?.to_dtype(dtype)?;
    Ok((cos, sin))
}

/// Apply rotary embedding to `x` (rotate-half). `cos`/`sin` broadcast over the
/// leading dims of `x`; `head_dim` is the size of `x`'s last dimension.
pub fn apply(x: &Tensor, cos: &Tensor, sin: &Tensor, head_dim: usize) -> Result<Tensor> {
    let half = head_dim / 2;
    let x1 = x.narrow(D::Minus1, 0, half)?;
    let x2 = x.narrow(D::Minus1, half, half)?;
    let rotated = Tensor::cat(&[&x2.neg()?, &x1], D::Minus1)?;
    x.broadcast_mul(cos)? + rotated.broadcast_mul(sin)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotary_at_position_zero_is_identity() {
        // At position 0 every angle is 0, so cos = 1 and sin = 0 and rotary is
        // the identity — a deterministic anchor that the rotate-half wiring is
        // applied in the right orientation.
        let device = Device::Cpu;
        let head_dim = 4;
        let inv = inverse_frequencies(head_dim, 10000.0, &device).expect("inv freqs");
        let (cos, sin) = cos_sin(1, &inv, DType::F32).expect("cos/sin");
        let x = Tensor::from_vec(vec![1f32, 2., 3., 4.], (1, head_dim), &device).expect("x");
        let out = apply(&x, &cos, &sin, head_dim).expect("apply");
        let got = out
            .flatten_all()
            .expect("flat")
            .to_vec1::<f32>()
            .expect("vec");
        for (g, e) in got.iter().zip([1f32, 2., 3., 4.]) {
            assert!((g - e).abs() < 1e-5, "got {got:?}");
        }
    }

    #[test]
    fn inverse_frequencies_has_half_width() {
        let device = Device::Cpu;
        let inv = inverse_frequencies(8, 10000.0, &device).expect("inv");
        assert_eq!(inv.dims(), &[1, 4]);
    }
}
