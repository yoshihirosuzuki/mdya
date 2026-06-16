//! Shared pooling / normalization for on-device embedders.
//!
//! Both the ModernBERT (ruri) and BERT (MiniLM) embedders mean-pool the last
//! hidden state over the attention mask and L2-normalize the result, so cosine
//! similarity is well-defined (the LanceDB index uses `DistanceType::Cosine`).

use candle_core::Tensor;

/// Mean-pool `hidden` (`[batch, seq_len, hidden_size]`) over the sequence
/// dimension, weighting by `attention_mask` (`[batch, seq_len]`, 1 = real,
/// 0 = pad) so padded positions do not contribute. Token counts are clamped to
/// at least 1 to avoid a divide-by-zero on an all-pad row.
pub(crate) fn mean_pool(
    hidden: &Tensor,
    attention_mask: &Tensor,
) -> Result<Tensor, candle_core::Error> {
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

/// L2-normalize each row of `rows` (a `[batch, hidden]` tensor; normalized
/// along dim 1, the hidden-size axis) so cosine similarity reduces to a dot
/// product. The norm is clamped away from zero to keep a zero row finite.
pub(crate) fn l2_normalize(rows: &Tensor) -> Result<Tensor, candle_core::Error> {
    let sq = rows.sqr()?;
    let sum = sq.sum_keepdim(1)?;
    let norm = sum.sqrt()?.clamp(1e-12_f32, f32::INFINITY)?;
    rows.broadcast_div(&norm)
}
