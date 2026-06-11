//! `mdya update-all` subcommand.
//!
//! Wires the config layer, the embedder, the ingest backend, and the
//! `indicatif`-based progress UI together. Backend logic lives in
//! `crate::ingest`; this module is the CLI-side coordinator + UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use thiserror::Error;
use tracing::warn;

use crate::config;
use crate::embedding::{EmbedError, Embedder, ModelCache, ModelCacheError, build_embedder};
use crate::ingest::{
    FileOutcome, IngestError, IngestProgress, UpdateSummary, update_all_collections,
};

/// `mdya update-all` entry point. Called from `cli::Cli::run`.
pub async fn run(
    config_dir_flag: Option<&Path>,
    model_cache_dir_flag: Option<&Path>,
) -> Result<(), UpdateAllError> {
    let base = config::resolve_config_dir(config_dir_flag)?;
    let cfg = config::load(&base.join("config.yml"))?;
    let collections = expand_collection_paths(&cfg);
    let cache = ModelCache::new(&config::resolve_model_cache_dir(model_cache_dir_flag)?)?;
    let parallelism = resolve_embed_parallelism(&cfg.runtime);
    let embedder: Arc<dyn Embedder> =
        build_embedder(&cfg.embedding.model, &cfg.embedding.ollama.endpoint, &cache).await?;
    let progress: Arc<dyn IngestProgress> =
        Arc::new(IndicatifProgress::with_parallelism(parallelism));
    let summary =
        update_all_collections(&collections, &base, embedder, progress, parallelism).await?;
    println!("{}", format_summary(&summary));
    if summary.failed > 0 {
        return Err(UpdateAllError::FilesFailed(summary.failed));
    }
    Ok(())
}

/// Resolve the effective `embed_parallelism` for an ingest run,
/// clamping the YAML value to the fixed `MAX_EMBED_PARALLELISM` sanity
/// ceiling (see `RuntimeConfig::embed_parallelism_capped`). Logs a
/// warning when the ceiling actually fires so the user notices a value
/// that would otherwise have built an unbounded futures stream. Shared
/// with `cli::vector` so `mdya vector use` clamps the same way.
pub(crate) fn resolve_embed_parallelism(runtime: &config::RuntimeConfig) -> usize {
    let configured = runtime.embed_parallelism;
    let capped = runtime.embed_parallelism_capped();
    if capped < configured {
        warn!(
            configured,
            capped,
            max = config::MAX_EMBED_PARALLELISM,
            "runtime.embed_parallelism exceeds the sanity ceiling; clamping"
        );
    }
    capped
}

/// `config.yml`'s `collections.<name>.path` may carry `~/`; resolve once
/// at the CLI boundary so `update_all_collections` works with concrete
/// `PathBuf`s and stays unaware of tilde expansion. Shared with
/// `cli::vector` (the `mdya vector use` re-embed reuses the same walk).
pub(crate) fn expand_collection_paths(cfg: &config::Config) -> BTreeMap<String, PathBuf> {
    cfg.collections
        .iter()
        .map(|(name, entry)| (name.clone(), config::expand_tilde(Path::new(&entry.path))))
        .collect()
}

fn format_summary(s: &UpdateSummary) -> String {
    // `N` is the count of files the walker visited (= new + updated +
    // skipped + failed). `removed` is the orphan-delete counter and lives
    // outside that sum.
    let total = s.new + s.updated + s.skipped + s.failed;
    format!(
        "Indexed {total} documents (new: {}, updated: {}, skipped: {}, removed: {}, failed: {}).",
        s.new, s.updated, s.skipped, s.removed, s.failed
    )
}

/// Live progress bar + N-spinner pool.
///
/// One main bar tracks file-completion count; a pool of `parallelism`
/// spinners shows the path of each in-flight file so the user can see
/// which files the parallel ingest is touching right now. With
/// `parallelism == 1` the pool collapses to a single spinner.
///
/// **Known limitation**: this
/// owns its own `MultiProgress`, separate from the one inside
/// `tracing-indicatif`'s `IndicatifLayer`. `IndicatifLayer::get_stderr_writer()`
/// only suspends bars owned by the layer's internal MP, so the bars
/// here can in theory be partially redrawn over by a `tracing::info!`
/// emitted in between two `inc(1)` calls. In practice `tracing::info!`
/// output is low-frequency during ingest and `indicatif` hides the
/// bars entirely when stderr is not a TTY (e.g. CI / piped output),
/// so the artifact is bounded to interactive sessions.
pub struct IndicatifProgress {
    /// File-completion counter; `inc(1)` per `finish_file` call.
    bar: ProgressBar,
    /// One spinner per slot; `start_file` writes the path here.
    spinners: Vec<ProgressBar>,
    /// `slots[i] == Some(path)` means `spinners[i]` is currently
    /// displaying `path`. `start_file` takes the first free slot;
    /// `finish_file` releases the slot whose `path` matches.
    slots: Mutex<Vec<Option<PathBuf>>>,
    // Held only to keep the MultiProgress alive (its draw target owns
    // the redraw thread). The trait methods drive `bar` / `spinners`
    // directly; `_multi` does not need to be touched after construction.
    _multi: MultiProgress,
}

impl IndicatifProgress {
    /// Single-spinner constructor kept for callers that do not parallelize.
    pub fn new() -> Self {
        Self::with_parallelism(1)
    }

    /// Build with `parallelism` spinners (one slot per in-flight file).
    /// A `parallelism` of `0` collapses to a single spinner so the UI
    /// still has something to draw when `runtime.embed_parallelism = 0`
    /// (= sequential ingest path).
    pub fn with_parallelism(parallelism: usize) -> Self {
        let pool_size = parallelism.max(1);
        let multi = MultiProgress::new();
        let bar = multi.add(ProgressBar::new(0));
        bar.set_style(
            ProgressStyle::with_template("[{bar:40}] {pos:>5}/{len:5} (ETA {eta})")
                .expect("hard-coded template parses"),
        );
        let spinners: Vec<ProgressBar> = (0..pool_size)
            .map(|_| {
                let s = multi.add(ProgressBar::new_spinner());
                s.set_style(
                    ProgressStyle::with_template("{spinner} {wide_msg}")
                        .expect("hard-coded template parses"),
                );
                s.enable_steady_tick(Duration::from_millis(100));
                s
            })
            .collect();
        let slots = Mutex::new(vec![None; pool_size]);
        Self {
            bar,
            spinners,
            slots,
            _multi: multi,
        }
    }
}

impl Default for IndicatifProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestProgress for IndicatifProgress {
    fn set_total_files(&self, total: usize) {
        self.bar.set_length(total as u64);
    }

    fn start_file(&self, path: &Path) {
        // `set_message` takes indicatif's own internal Mutex, so we
        // release the slots lock first to keep lock-acquire order
        // (slots → indicatif) consistent everywhere and avoid the
        // possibility of a future caller deadlocking us.
        let idx = {
            let mut slots = self.slots.lock().expect("slots mutex poisoned");
            let Some(idx) = slots.iter().position(Option::is_none) else {
                // All slots busy. `buffer_unordered(parallelism)` upstream
                // caps in-flight files at `pool_size`, so the pool always
                // has a free slot — falling through here would mean a
                // contract violation. Log and skip the spinner update so
                // the run still completes.
                warn!(
                    path = %path.display(),
                    pool_size = self.spinners.len(),
                    "ingest progress: spinner pool exhausted, skipping update",
                );
                return;
            };
            slots[idx] = Some(path.to_path_buf());
            idx
        };
        self.spinners[idx].set_message(path.display().to_string());
    }

    fn finish_file(&self, path: &Path, _outcome: FileOutcome) {
        // Same lock-order discipline as `start_file`: release slots
        // before touching indicatif's internal Mutex.
        let released_idx = {
            let mut slots = self.slots.lock().expect("slots mutex poisoned");
            slots
                .iter()
                .position(|s| s.as_deref() == Some(path))
                .inspect(|&idx| slots[idx] = None)
        };
        if let Some(idx) = released_idx {
            self.spinners[idx].set_message(String::new());
        } else {
            // No matching slot: either `start_file` was never called
            // for this path (caller bug) or the pool overflowed during
            // `start_file`.
            warn!(
                path = %path.display(),
                "ingest progress: finish_file with no matching slot",
            );
        }
        // Tick unconditionally so the total count stays accurate even
        // when the slot was missing (pool overflow path or caller bug).
        self.bar.inc(1);
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
        for s in &self.spinners {
            s.finish_and_clear();
        }
    }
}

#[derive(Debug, Error)]
pub enum UpdateAllError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error(transparent)]
    ModelCache(#[from] ModelCacheError),

    #[error(transparent)]
    Embedding(#[from] EmbedError),

    #[error(transparent)]
    Ingest(#[from] IngestError),

    /// At least one file produced a per-file error during ingest. The
    /// `update_all_collections` summary line was still printed to stdout
    /// so the user sees which counter is non-zero; this variant exists
    /// so `main` propagates exit-1 to the shell (grill Q5). The
    /// per-file `tracing::warn!` lines emitted during ingest carry the
    /// specific path / reason for each failure.
    #[error("{0} file(s) failed during ingest — check warnings above for details")]
    FilesFailed(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(new: u64, updated: u64, skipped: u64, removed: u64, failed: u64) -> UpdateSummary {
        UpdateSummary {
            new,
            updated,
            skipped,
            removed,
            failed,
        }
    }

    #[test]
    fn empty_summary_renders_with_all_zeros() {
        let out = format_summary(&s(0, 0, 0, 0, 0));
        assert_eq!(
            out,
            "Indexed 0 documents (new: 0, updated: 0, skipped: 0, removed: 0, failed: 0)."
        );
    }

    #[test]
    fn mixed_summary_renders_total_as_new_plus_updated_plus_skipped_plus_failed() {
        let out = format_summary(&s(5, 3, 34, 2, 1));
        // total = 5 + 3 + 34 + 1 = 43; removed (2) does NOT count toward total.
        assert_eq!(
            out,
            "Indexed 43 documents (new: 5, updated: 3, skipped: 34, removed: 2, failed: 1)."
        );
    }

    #[test]
    fn indicatif_progress_starts_in_a_clean_state() {
        // `IndicatifProgress::new` must not panic — the templates are
        // hard-coded so any future typo trips this immediately.
        let p = IndicatifProgress::new();
        p.set_total_files(0);
        p.finish();
    }

    #[test]
    fn indicatif_progress_writes_path_into_spinner_message() {
        let p = IndicatifProgress::new();
        p.set_total_files(1);
        p.start_file(Path::new("notes/foo.md"));
        // Single-spinner mode collapses to `spinners[0]`.
        assert!(p.spinners[0].message().contains("foo.md"));
        p.finish();
    }

    #[test]
    fn indicatif_progress_with_parallelism_allocates_n_spinners() {
        let p = IndicatifProgress::with_parallelism(4);
        assert_eq!(p.spinners.len(), 4);
        assert_eq!(
            p.slots.lock().expect("slots mutex").len(),
            4,
            "slot count must match spinner count",
        );
        p.finish();
    }

    #[test]
    fn indicatif_progress_zero_parallelism_collapses_to_single_spinner() {
        // `runtime.embed_parallelism = 0` is the sequential-path sentinel;
        // the UI still needs one spinner so `start_file` has somewhere to
        // write.
        let p = IndicatifProgress::with_parallelism(0);
        assert_eq!(p.spinners.len(), 1);
        p.finish();
    }

    #[test]
    fn indicatif_progress_concurrent_start_file_uses_separate_slots() {
        let p = IndicatifProgress::with_parallelism(2);
        p.start_file(Path::new("a.md"));
        p.start_file(Path::new("b.md"));
        let slots = p.slots.lock().expect("slots mutex");
        assert_eq!(slots[0].as_deref(), Some(Path::new("a.md")));
        assert_eq!(slots[1].as_deref(), Some(Path::new("b.md")));
        drop(slots);
        p.finish();
    }

    #[test]
    fn indicatif_progress_pool_exhaustion_warns_and_does_not_panic() {
        // Exceeding the pool size must not panic; the spinner update is
        // skipped (warn) but `finish` still cleans up gracefully.
        let p = IndicatifProgress::with_parallelism(1);
        p.start_file(Path::new("a.md"));
        // Second call has no free slot. Should warn + skip, not panic.
        p.start_file(Path::new("b.md"));
        let slots = p.slots.lock().expect("slots mutex");
        assert_eq!(
            slots[0].as_deref(),
            Some(Path::new("a.md")),
            "the existing slot must keep its path",
        );
        drop(slots);
        p.finish();
    }

    #[test]
    fn indicatif_progress_finish_file_no_matching_slot_increments_bar() {
        // `finish_file` called without a paired `start_file` must still
        // tick the bar so the completion count stays accurate.
        let p = IndicatifProgress::with_parallelism(1);
        p.set_total_files(1);
        p.finish_file(Path::new("ghost.md"), FileOutcome::Failed);
        assert_eq!(p.bar.position(), 1, "bar must tick even on no-match");
        p.finish();
    }

    #[test]
    fn indicatif_progress_finish_file_releases_matching_slot() {
        let p = IndicatifProgress::with_parallelism(2);
        p.start_file(Path::new("a.md"));
        p.start_file(Path::new("b.md"));
        p.finish_file(Path::new("a.md"), FileOutcome::New);
        let slots = p.slots.lock().expect("slots mutex");
        assert!(slots[0].is_none(), "a.md slot must be released");
        assert_eq!(
            slots[1].as_deref(),
            Some(Path::new("b.md")),
            "b.md slot must remain occupied",
        );
        drop(slots);
        p.finish();
    }
}
