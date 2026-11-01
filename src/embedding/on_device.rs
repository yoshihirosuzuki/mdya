//! The models run on this PC: fetching them into the embedding model cache
//! directory, loading them, and making embedding vectors with them on the
//! CPU.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationDirection,
    TruncationParams, TruncationStrategy,
};

use super::EmbeddingError;
use super::prepared::{ModelPreparation, PreparedModel};
use super::supported::OnDeviceModel;
use crate::types::{BoxError, ChunkEmbedding, EmbeddingVector};
use encoder::{Architecture, Encoder};
use fetch::Hub;

mod encoder;
mod fetch;

// Tests that fetch and run the real models, run by hand.
#[cfg(test)]
mod real_model_tests;

/// The files every model needs.
const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// The text embedded once to learn the model's dimension.
const DIMENSION_PROBE: &str = "dimension";

/// What the component needs to know to run a model.
#[derive(Clone, Copy, Debug)]
pub(super) struct ModelSpec {
    /// The Hugging Face revision (a commit) the files are fetched at, so that
    /// a model is always the same files.
    revision: &'static str,
    architecture: Architecture,
    /// Put before a search query, as the model expects.
    query_prefix: &'static str,
    /// Put before a chunk, as the model expects.
    document_prefix: &'static str,
    /// The context length, in tokens: the `max_seq_length` that the model's
    /// sentence-transformers configuration gives.
    context_length: usize,
    /// The smallest number of chunks per call whose time per chunk was within
    /// 5% of the fastest measured (`real_model_tests::measure_chunks_per_call`,
    /// on an Apple M4 with 10 cores).
    /// The memory of a call grows with its chunks, and several calls run at
    /// once, so a larger number would add memory while saving at most 5% of
    /// the time per chunk.
    pub(super) max_chunks_per_call: NonZeroUsize,
}

#[expect(clippy::expect_used)]
const fn chunks(count: usize) -> NonZeroUsize {
    NonZeroUsize::new(count).expect("the limits below are not zero")
}

/// The spec of `model`.
pub(super) fn spec(model: OnDeviceModel) -> ModelSpec {
    match model {
        // https://huggingface.co/cl-nagoya/ruri-v3-30m: "検索クエリ: " for
        // queries and "検索文書: " for documents in retrieval tasks.
        OnDeviceModel::RuriV3_30m => ModelSpec {
            revision: "24899e5de370b56d179604a007c0d727bf144504",
            architecture: Architecture::ModernBert,
            query_prefix: "検索クエリ: ",
            document_prefix: "検索文書: ",
            context_length: 8192,
            max_chunks_per_call: chunks(4),
        },
        OnDeviceModel::AllMiniLmL6V2 => ModelSpec {
            revision: "1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
            architecture: Architecture::Bert,
            query_prefix: "",
            document_prefix: "",
            context_length: 256,
            max_chunks_per_call: chunks(16),
        },
        OnDeviceModel::EmbeddingGemma300m => todo!("google/embeddinggemma-300m"),
    }
}

/// Prepares a model run on this PC: fetches its files unless they are in the
/// cache, loads it, and learns its dimension by embedding a short text.
pub(super) struct OnDevicePreparation {
    model: OnDeviceModel,
    cache_dir: PathBuf,
    hub: HubSettings,
}

/// Where to fetch from, and with which token.
pub(super) struct HubSettings {
    pub(super) base: String,
    pub(super) token: Option<String>,
}

impl HubSettings {
    /// Hugging Face itself.
    pub(super) fn hugging_face(token: Option<String>) -> Self {
        Self {
            base: fetch::HUGGING_FACE.to_owned(),
            token,
        }
    }
}

impl OnDevicePreparation {
    pub(super) fn new(model: OnDeviceModel, cache_dir: PathBuf, hub: HubSettings) -> Self {
        Self {
            model,
            cache_dir,
            hub,
        }
    }
}

#[async_trait]
impl ModelPreparation for OnDevicePreparation {
    async fn prepare(&self) -> Result<Arc<dyn PreparedModel>, EmbeddingError> {
        let spec = spec(self.model);
        let hub = Hub::connect(&self.hub.base, self.hub.token.clone()).map_err(unavailable)?;
        let dir = hub
            .fetch(
                &self.cache_dir,
                self.model.repository(),
                spec.revision,
                &FILES,
            )
            .await
            .map_err(unavailable)?;
        let runner = run_blocking(move || Runner::load(&dir, &spec)).await?;
        LoadedModel::with_probed_dimension(Arc::new(runner), spec).await
    }
}

/// A loaded model: the tokenizer and the network.
struct Runner {
    tokenizer: Tokenizer,
    encoder: Box<dyn Encoder>,
}

/// Texts split into tokens, cut to the context length.
struct Tokens {
    ids: Tensor,
    mask: Tensor,
    /// For each text, whether it fit the context length.
    fits: Vec<bool>,
}

impl Runner {
    fn load(dir: &Path, spec: &ModelSpec) -> Result<Self, BoxError> {
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
        let pad_id = config
            .get("pad_token_id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or(MissingPadToken)?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))?;
        Ok(Self {
            tokenizer: configured(tokenizer, spec.context_length, pad_id)?,
            encoder: encoder::load(spec.architecture, dir)?,
        })
    }

    /// Splits `texts` into tokens, cutting each at the context length.
    fn tokens(&self, texts: Vec<String>) -> Result<Tokens, BoxError> {
        let encodings = self.tokenizer.encode_batch(texts, true)?;
        let fits = encodings
            .iter()
            .map(|encoding| encoding.get_overflowing().is_empty())
            .collect();
        let width = encodings.first().map_or(0, |encoding| encoding.len());
        let mut ids = Vec::with_capacity(encodings.len() * width);
        let mut mask = Vec::with_capacity(encodings.len() * width);
        for encoding in &encodings {
            ids.extend_from_slice(encoding.get_ids());
            mask.extend_from_slice(encoding.get_attention_mask());
        }
        let shape = (encodings.len(), width);
        Ok(Tokens {
            ids: Tensor::from_vec(ids, shape, &Device::Cpu)?,
            mask: Tensor::from_vec(mask, shape, &Device::Cpu)?,
            fits,
        })
    }

    /// The embedding vector of each text in `tokens`: the mean of the hidden
    /// states of its tokens, scaled to length 1.
    fn vectors(&self, tokens: &Tokens) -> Result<Vec<EmbeddingVector>, BoxError> {
        let hidden = self.encoder.hidden_states(&tokens.ids, &tokens.mask)?;
        Ok(mean_and_normalize(&hidden, &tokens.mask)?.to_vec2::<f32>()?)
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<ChunkEmbedding>, BoxError> {
        let tokens = self.tokens(texts)?;
        let vectors = self.vectors(&tokens)?;
        Ok(vectors
            .into_iter()
            .zip(tokens.fits)
            .map(|(vector, covers_whole_chunk)| ChunkEmbedding {
                vector,
                covers_whole_chunk,
            })
            .collect())
    }

    /// The vector of `query`, or `None` when it exceeds the context length,
    /// in which case the network is not run.
    fn embed_query(&self, query: String) -> Result<Option<EmbeddingVector>, BoxError> {
        let tokens = self.tokens(vec![query])?;
        if tokens.fits.contains(&false) {
            return Ok(None);
        }
        let vector = self.vectors(&tokens)?.into_iter().next().ok_or(NoVector)?;
        Ok(Some(vector))
    }
}

/// `tokenizer`, set to cut each text at `context_length` tokens (counting the
/// special tokens the model adds) and to pad the texts of a call to the
/// longest, with `pad_id`. These replace whatever the file set.
fn configured(
    mut tokenizer: Tokenizer,
    context_length: usize,
    pad_id: u32,
) -> Result<Tokenizer, BoxError> {
    let pad_token = tokenizer.id_to_token(pad_id).ok_or(MissingPadToken)?;
    tokenizer.with_truncation(Some(TruncationParams {
        direction: TruncationDirection::Right,
        max_length: context_length,
        strategy: TruncationStrategy::LongestFirst,
        stride: 0,
    }))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        direction: PaddingDirection::Right,
        pad_to_multiple_of: None,
        pad_id,
        pad_type_id: 0,
        pad_token,
    }));
    Ok(tokenizer)
}

/// The mean of `hidden` (texts, tokens, hidden) over the tokens that `mask`
/// (texts, tokens) marks, each row scaled to length 1. Scaling does not change
/// the cosine similarity that search compares vectors by, and makes the vectors
/// of every model alike, whether or not its own pipeline scales them.
fn mean_and_normalize(hidden: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
    let weights = mask.to_dtype(DType::F32)?.unsqueeze(2)?;
    let summed = hidden.broadcast_mul(&weights)?.sum(1)?;
    let counts = weights.sum(1)?.clamp(1.0_f32, f32::INFINITY)?;
    let means = summed.broadcast_div(&counts)?;
    let lengths = means
        .sqr()?
        .sum_keepdim(1)?
        .sqrt()?
        .clamp(1e-12_f32, f32::INFINITY)?;
    means.broadcast_div(&lengths)
}

/// A model ready to make embedding vectors.
struct LoadedModel {
    runner: Arc<Runner>,
    spec: ModelSpec,
    dimension: usize,
}

impl LoadedModel {
    async fn with_probed_dimension(
        runner: Arc<Runner>,
        spec: ModelSpec,
    ) -> Result<Arc<dyn PreparedModel>, EmbeddingError> {
        let probe = runner.clone();
        let embeddings =
            run_blocking(move || probe.embed(vec![DIMENSION_PROBE.to_owned()])).await?;
        let dimension = embeddings
            .first()
            .map_or(0, |embedding| embedding.vector.len());
        if dimension == 0 {
            return Err(unavailable(Box::new(NoVector)));
        }
        Ok(Arc::new(Self {
            runner,
            spec,
            dimension,
        }))
    }
}

#[async_trait]
impl PreparedModel for LoadedModel {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        let prefixed = texts
            .iter()
            .map(|text| format!("{}{text}", self.spec.document_prefix))
            .collect();
        let runner = self.runner.clone();
        run_blocking(move || runner.embed(prefixed)).await
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        let prefixed = format!("{}{query}", self.spec.query_prefix);
        let runner = self.runner.clone();
        run_blocking(move || runner.embed_query(prefixed))
            .await?
            .ok_or(EmbeddingError::QueryExceedsContextLength)
    }
}

/// Runs `work` on tokio's blocking threads, so that the computation does not
/// hold up other asynchronous work, and waits for it. A panic in `work`
/// reaches the caller as a panic.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, BoxError> + Send + 'static,
) -> Result<T, EmbeddingError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(unavailable),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(unavailable(Box::new(error))),
    }
}

fn unavailable(source: BoxError) -> EmbeddingError {
    EmbeddingError::ModelUnavailable { source }
}

#[derive(Debug, thiserror::Error)]
#[error("the model's configuration names no padding token that its tokenizer has")]
struct MissingPadToken;

#[derive(Debug, thiserror::Error)]
#[error("the model made no embedding vector")]
struct NoVector;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A tokenizer that splits only on whitespace, knows a few words, and puts
    /// `[CLS]` before and `[SEP]` after each text.
    const TOKENIZER: &str = r#"{
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": null,
        "pre_tokenizer": {"type": "WhitespaceSplit"},
        "post_processor": {
            "type": "TemplateProcessing",
            "single": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}}
            ],
            "pair": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}},
                {"Sequence": {"id": "B", "type_id": 1}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 1}}
            ],
            "special_tokens": {
                "[CLS]": {"id": "[CLS]", "ids": [2], "tokens": ["[CLS]"]},
                "[SEP]": {"id": "[SEP]", "ids": [3], "tokens": ["[SEP]"]}
            }
        },
        "decoder": null,
        "model": {
            "type": "WordLevel",
            "vocab": {
                "[PAD]": 0, "[UNK]": 1, "[CLS]": 2, "[SEP]": 3,
                "query:": 4, "doc:": 5, "a": 6, "b": 7, "c": 8
            },
            "unk_token": "[UNK]"
        }
    }"#;

    const PAD: u32 = 0;
    const CLS: u32 = 2;
    const SEP: u32 = 3;
    const QUERY: u32 = 4;
    const DOC: u32 = 5;
    const A: u32 = 6;
    const B: u32 = 7;

    /// Stands in for a network: the hidden state of token `id` is
    /// `[id, 1]`. Records the ids of each call, and panics when told to.
    #[derive(Default)]
    struct TestEncoder {
        calls: Mutex<Vec<Vec<Vec<u32>>>>,
        panics: AtomicBool,
    }

    impl Encoder for Arc<TestEncoder> {
        fn hidden_states(&self, ids: &Tensor, _mask: &Tensor) -> candle_core::Result<Tensor> {
            if self.panics.swap(false, Ordering::SeqCst) {
                panic!("the network panicked");
            }
            self.calls.lock().unwrap().push(ids.to_vec2::<u32>()?);
            let ids = ids.to_dtype(DType::F32)?.unsqueeze(2)?;
            Tensor::cat(&[&ids, &ids.ones_like()?], 2)
        }
    }

    fn spec(context_length: usize) -> ModelSpec {
        ModelSpec {
            revision: "test",
            architecture: Architecture::Bert,
            query_prefix: "query: ",
            document_prefix: "doc: ",
            context_length,
            max_chunks_per_call: chunks(8),
        }
    }

    async fn model(context_length: usize) -> (Arc<dyn PreparedModel>, Arc<TestEncoder>) {
        let tokenizer = Tokenizer::from_bytes(TOKENIZER).unwrap();
        let encoder = Arc::new(TestEncoder::default());
        let runner = Runner {
            tokenizer: configured(tokenizer, context_length, PAD).unwrap(),
            encoder: Box::new(encoder.clone()),
        };
        let model = LoadedModel::with_probed_dimension(Arc::new(runner), spec(context_length))
            .await
            .unwrap();
        encoder.calls.lock().unwrap().clear();
        (model, encoder)
    }

    fn normalized(vector: &[f32]) -> Vec<f32> {
        let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        vector.iter().map(|x| x / length).collect()
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-6, "{actual:?} != {expected:?}");
        }
    }

    #[tokio::test]
    async fn learns_the_dimension_by_embedding_a_short_text() {
        let (model, _) = model(16).await;

        assert_eq!(model.dimension(), 2);
    }

    #[tokio::test]
    async fn puts_the_document_prefix_before_chunks_and_the_query_prefix_before_queries() {
        let (model, encoder) = model(16).await;

        model.embed_chunks(&["a"]).await.unwrap();
        model.embed_query("b").await.unwrap();

        assert_eq!(
            *encoder.calls.lock().unwrap(),
            [vec![vec![CLS, DOC, A, SEP]], vec![vec![CLS, QUERY, B, SEP]]]
        );
    }

    #[tokio::test]
    async fn makes_the_mean_of_the_tokens_without_the_padding_scaled_to_length_one() {
        let (model, encoder) = model(16).await;

        let embeddings = model.embed_chunks(&["a", "a b a"]).await.unwrap();

        // The first text is padded to the length of the second.
        assert_eq!(
            encoder.calls.lock().unwrap()[0],
            [
                vec![CLS, DOC, A, SEP, PAD, PAD],
                vec![CLS, DOC, A, B, A, SEP]
            ]
        );
        let mean = |ids: &[u32]| {
            let sum: f32 = ids.iter().map(|id| *id as f32).sum();
            normalized(&[sum / ids.len() as f32, 1.0])
        };
        assert_close(&embeddings[0].vector, &mean(&[CLS, DOC, A, SEP]));
        assert_close(&embeddings[1].vector, &mean(&[CLS, DOC, A, B, A, SEP]));
        assert!(
            embeddings
                .iter()
                .all(|embedding| embedding.covers_whole_chunk)
        );
    }

    #[tokio::test]
    async fn embeds_the_beginning_of_a_chunk_that_exceeds_the_context_length() {
        // [CLS] doc: a b [SEP] is 5 tokens.
        let (model, encoder) = model(5).await;

        let embeddings = model.embed_chunks(&["a b", "a b a"]).await.unwrap();

        assert!(embeddings[0].covers_whole_chunk);
        assert!(!embeddings[1].covers_whole_chunk);
        assert_eq!(
            encoder.calls.lock().unwrap()[0],
            [vec![CLS, DOC, A, B, SEP], vec![CLS, DOC, A, B, SEP]]
        );
    }

    #[tokio::test]
    async fn refuses_a_query_that_exceeds_the_context_length_without_running_the_network() {
        let (model, encoder) = model(5).await;

        let result = model.embed_query("a b a").await;

        assert!(matches!(
            result,
            Err(EmbeddingError::QueryExceedsContextLength)
        ));
        assert!(encoder.calls.lock().unwrap().is_empty());
        assert!(model.embed_query("a b").await.is_ok());
    }

    #[tokio::test]
    async fn passes_a_panic_in_the_network_on_and_stays_usable() {
        let (model, encoder) = model(16).await;
        encoder.panics.store(true, Ordering::SeqCst);

        let first = tokio::spawn({
            let model = model.clone();
            async move { model.embed_chunks(&["a"]).await }
        })
        .await;

        assert!(first.unwrap_err().is_panic());
        assert!(model.embed_chunks(&["a"]).await.is_ok());
    }

    #[test]
    fn refuses_a_tokenizer_without_the_padding_token_of_the_configuration() {
        let tokenizer = Tokenizer::from_bytes(TOKENIZER).unwrap();

        let error = configured(tokenizer, 16, 99).err().unwrap();

        assert_eq!(
            error.to_string(),
            "the model's configuration names no padding token that its tokenizer has"
        );
        assert!(error.source().is_none());
    }
}
