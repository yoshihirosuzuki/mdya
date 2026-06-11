//! YAML schema types for `~/.mdya/config.yml`.
//!
//! `Config` is an information holder; `ConfigStore` (sibling module) carries
//! the load/save behavior. Chunking is fixed in source rather than user-
//! configurable, so the schema has no `chunking` section. The remaining
//! sections are `collections` / `embedding` / `runtime`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ConfigError;
use crate::embedding::RURI_V3_30M_MODEL_ID;

/// Default for `runtime.memory_limit_mb`. 8192 MB caps a 16-32 GB
/// development machine at roughly half its RAM, leaving headroom for
/// the OS and other processes so the watchdog kicks in before swap
/// thrashing hangs the box.
pub const DEFAULT_MEMORY_LIMIT_MB: u64 = 8192;

/// Default for `runtime.embed_parallelism`. 8 in-flight files is a
/// conservative starting point — tune up or down to match available
/// cores and `memory_limit_mb`. The product
/// `embed_parallelism × per-file embed peak (≈ 1.5 GB worst case)`
/// should stay under `memory_limit_mb`; the memory watchdog kills
/// the process otherwise. `0` disables file parallelism (sequential
/// path).
pub const DEFAULT_EMBED_PARALLELISM: usize = 8;

/// Sanity upper bound on `runtime.embed_parallelism`. The figure is
/// large enough that no realistic deployment touches it; it exists
/// purely as a `buffer_unordered` ceiling so a typo or runaway value
/// in `config.yml` (e.g. `embed_parallelism: 99999`) cannot tee up an
/// unbounded futures stream that would OOM before the runtime memory
/// guard fires. Memory budget enforcement remains the
/// `memory_limit_mb` watchdog's job; this cap only prevents the kind
/// of value that the runtime data structures themselves would refuse.
pub const MAX_EMBED_PARALLELISM: usize = 1024;

/// Default endpoint when `embedding.ollama.endpoint` is omitted from
/// `config.yml`. Mirrors the loopback address Ollama itself binds at
/// install time, so a co-located Ollama process on the same machine
/// works out of the box without any YAML override.
pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";

/// Root of `config.yml`. See `docs/manual/en/configuration.md` for the
/// user-facing field reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Collection name (free string) → entry. `BTreeMap` keeps `mdya init`
    /// output deterministic (alphabetical key order in YAML).
    #[serde(default)]
    pub collections: BTreeMap<String, CollectionEntry>,

    pub embedding: EmbeddingConfig,

    /// Runtime policy. The whole section is `#[serde(default)]` so a
    /// `config.yml` without `runtime:` still gets the 8192 MB cap applied
    /// silently on next load. `0` disables the guard.
    #[serde(default)]
    pub runtime: RuntimeConfig,
}

/// One row under `collections`. `path` stays as the user-typed string
/// (possibly `~/`) and is resolved at read time, not at parse time, so the
/// YAML round-trips losslessly when `mdya collection add` rewrites the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionEntry {
    pub path: String,

    /// Set via `mdya collection add --description`. Surfaced by
    /// `mdya collection list` and `mdya status`. Kept off the YAML
    /// when unset; the file stays clean for collections without
    /// descriptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub model: String,

    /// Ollama backend configuration. The whole subsection is omitted
    /// from `mdya init`'s template output when it carries the default
    /// loopback endpoint, so users who stay on the candle path never
    /// see an `ollama:` key in their `config.yml`. `ollama:<model>`
    /// users either inherit the default loopback endpoint or write an
    /// explicit override here.
    #[serde(default, skip_serializing_if = "OllamaConfig::is_default")]
    pub ollama: OllamaConfig,
}

/// Ollama-backend-specific configuration. Only `endpoint` is wired
/// today; future Ollama-specific knobs land here so they cluster under
/// one YAML subsection instead of polluting the top-level `embedding`
/// shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// Base URL the Ollama embedding backend posts to. Default is the
    /// loopback address Ollama itself binds at install time. Overriding
    /// this to a non-loopback host means embedding text leaves the
    /// device, which violates mdya's "inference stays local" property —
    /// the override is honoured (no code guard) but users opting in
    /// must accept that trade-off themselves.
    #[serde(default = "default_ollama_endpoint")]
    pub endpoint: String,
}

impl OllamaConfig {
    fn is_default(&self) -> bool {
        self.endpoint == DEFAULT_OLLAMA_ENDPOINT
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_OLLAMA_ENDPOINT.to_string(),
        }
    }
}

fn default_ollama_endpoint() -> String {
    DEFAULT_OLLAMA_ENDPOINT.to_string()
}

/// Process-level runtime policy. Holds the safety knobs
/// that bound how much the ingest pipeline is allowed to consume — memory
/// (`memory_limit_mb`) and concurrent embed work (`embed_parallelism`).
/// Future runtime knobs land here without disturbing other top-level keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Resident-set memory cap in MB. `0` disables the watchdog entirely
    /// (POSIX `ulimit 0` convention). When omitted from YAML the struct-level
    /// `Default` applies, which yields [`DEFAULT_MEMORY_LIMIT_MB`].
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,

    /// Number of files embedded in parallel during `mdya update-all`. `0`
    /// disables file parallelism and runs the sequential path. The product
    /// of `embed_parallelism × per-file embed peak (≈ 1.5 GB worst case,
    /// writer.rs::EMBED_BATCH_SIZE comment)` should stay under
    /// `memory_limit_mb`; the watchdog kills the process otherwise.
    #[serde(default = "default_embed_parallelism")]
    pub embed_parallelism: usize,
}

impl RuntimeConfig {
    /// Effective `embed_parallelism` after applying the
    /// `MAX_EMBED_PARALLELISM` sanity ceiling. The cap is a fixed
    /// constant (not derived from any other config field), so the
    /// value the user wrote in `config.yml` is honoured exactly until
    /// it crosses the static ceiling. `0` (the explicit sequential
    /// path) is preserved verbatim. Memory budget enforcement is the
    /// `memory_limit_mb` watchdog's job and is independent of this cap.
    pub fn embed_parallelism_capped(&self) -> usize {
        if self.embed_parallelism == 0 {
            return 0;
        }
        self.embed_parallelism.min(MAX_EMBED_PARALLELISM)
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit_mb: DEFAULT_MEMORY_LIMIT_MB,
            embed_parallelism: DEFAULT_EMBED_PARALLELISM,
        }
    }
}

fn default_memory_limit_mb() -> u64 {
    DEFAULT_MEMORY_LIMIT_MB
}

fn default_embed_parallelism() -> usize {
    DEFAULT_EMBED_PARALLELISM
}

impl Config {
    /// The `chunking` section is intentionally absent because chunking is
    /// fixed in source rather than user-configurable.
    pub fn init_template() -> Self {
        Self {
            collections: BTreeMap::new(),
            embedding: EmbeddingConfig {
                model: RURI_V3_30M_MODEL_ID.to_string(),
                ollama: OllamaConfig::default(),
            },
            runtime: RuntimeConfig::default(),
        }
    }
}

/// Validate that a collection name is safe to splice into a LanceDB
/// SQL predicate without injection risk and is reasonable as a config
/// identifier. Accepts `[A-Za-z0-9_-]`, 1..=64 characters. Used by
/// `mdya collection add --name <NAME>` and by the YAML loader so the
/// ingest writer can splice the name directly into `Table::delete` /
/// `UpdateBuilder::only_if` predicates (lancedb 0.29 has no typed
/// expression API on the write side).
pub fn validate_collection_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::InvalidCollectionName {
            name: name.to_string(),
            reason: "must not be empty",
        });
    }
    if name.len() > 64 {
        return Err(ConfigError::InvalidCollectionName {
            name: name.to_string(),
            reason: "must be 64 characters or fewer",
        });
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::InvalidCollectionName {
            name: name.to_string(),
            reason: "must contain only ASCII letters, digits, '-' or '_'",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_template_pins_the_default_embedding_model() {
        let cfg = Config::init_template();
        assert_eq!(cfg.embedding.model, "cl-nagoya/ruri-v3-30m");
        assert!(cfg.collections.is_empty());
    }

    #[test]
    fn init_template_omits_chunking_section() {
        // chunking is fixed in source; surfacing a `chunking:` key in the
        // template would mislead users into expecting a knob that does not
        // exist.
        let cfg = Config::init_template();
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            !yaml.contains("chunking"),
            "init template must not mention chunking; got:\n{yaml}"
        );
    }

    #[test]
    fn yaml_round_trip_preserves_tilde_path_as_string() {
        let mut cfg = Config::init_template();
        cfg.collections.insert(
            "notes".to_string(),
            CollectionEntry {
                path: "~/notes".to_string(),
                description: None,
            },
        );

        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let back: Config = serde_yaml_ng::from_str(&yaml).expect("deserialize");

        assert_eq!(back.collections["notes"].path, "~/notes");
        assert_eq!(back, cfg);
    }

    #[test]
    fn collection_entry_without_description_omits_the_key_from_yaml() {
        let mut cfg = Config::init_template();
        cfg.collections.insert(
            "notes".to_string(),
            CollectionEntry {
                path: "~/notes".to_string(),
                description: None,
            },
        );
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            !yaml.contains("description"),
            "unset description must not appear in YAML; got:\n{yaml}"
        );
    }

    #[test]
    fn collection_entry_description_round_trips() {
        let mut cfg = Config::init_template();
        cfg.collections.insert(
            "notes".to_string(),
            CollectionEntry {
                path: "~/notes".to_string(),
                description: Some("個人メモ".to_string()),
            },
        );
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let back: Config = serde_yaml_ng::from_str(&yaml).expect("deserialize");
        assert_eq!(
            back.collections["notes"].description.as_deref(),
            Some("個人メモ")
        );
        assert_eq!(back, cfg);
    }

    #[test]
    fn legacy_collection_without_description_deserialises_to_none() {
        let yaml = "\
collections:
  notes:
    path: ~/notes
embedding:
  model: cl-nagoya/ruri-v3-30m
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.collections["notes"].description, None);
    }

    #[test]
    fn init_template_pins_the_default_memory_limit() {
        let cfg = Config::init_template();
        assert_eq!(cfg.runtime.memory_limit_mb, 8192);
    }

    #[test]
    fn yaml_without_runtime_section_falls_back_to_struct_default() {
        // Loading a config.yml without `runtime:` must still pin the 8192 cap
        // so the PC-hang guard is on by default, even when the user has not
        // opted in explicitly.
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.runtime.memory_limit_mb, 8192);
    }

    #[test]
    fn yaml_with_memory_limit_zero_round_trips_as_disable_sentinel() {
        // Users disable the guard by setting `memory_limit_mb: 0`; that
        // edit must survive a write-read cycle.
        let mut cfg = Config::init_template();
        cfg.runtime.memory_limit_mb = 0;

        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let back: Config = serde_yaml_ng::from_str(&yaml).expect("deserialize");

        assert_eq!(back.runtime.memory_limit_mb, 0);
    }

    #[test]
    fn yaml_with_explicit_memory_limit_overrides_struct_default() {
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 16384
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.runtime.memory_limit_mb, 16384);
    }

    #[test]
    fn init_template_pins_the_embed_parallelism_default() {
        let cfg = Config::init_template();
        assert_eq!(cfg.runtime.embed_parallelism, 8);
    }

    #[test]
    fn yaml_without_embed_parallelism_falls_back_to_struct_default() {
        // `embed_parallelism`'s missing-key default must apply silently, the
        // same way `memory_limit_mb` does.
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime: {}
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.runtime.embed_parallelism, 8);
    }

    #[test]
    fn yaml_with_embed_parallelism_zero_round_trips_as_disable_sentinel() {
        // Users disable parallelism by setting `embed_parallelism: 0`; that
        // edit must survive a write-read cycle.
        let mut cfg = Config::init_template();
        cfg.runtime.embed_parallelism = 0;

        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let back: Config = serde_yaml_ng::from_str(&yaml).expect("deserialize");

        assert_eq!(back.runtime.embed_parallelism, 0);
    }

    #[test]
    fn yaml_with_explicit_embed_parallelism_overrides_struct_default() {
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 16
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.runtime.embed_parallelism, 16);
    }

    #[test]
    fn yaml_with_legacy_chunking_section_is_silently_ignored() {
        // The schema must tolerate extra unknown keys (e.g. an unused
        // `chunking:` section); strict deserialization would break configs
        // that drift from the current shape.
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
chunking:
  strategy: fixed-window-by-bytes
  params:
    window_size: 512
    overlap: 64
runtime:
  memory_limit_mb: 8192
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.embedding.model, "cl-nagoya/ruri-v3-30m");
        assert_eq!(cfg.runtime.memory_limit_mb, 8192);
    }

    #[test]
    fn validate_collection_name_accepts_alnum_dash_underscore() {
        assert!(validate_collection_name("notes").is_ok());
        assert!(validate_collection_name("work-notes").is_ok());
        assert!(validate_collection_name("rag_v2").is_ok());
        assert!(validate_collection_name("A1-b2_C3").is_ok());
    }

    #[test]
    fn validate_collection_name_rejects_empty() {
        let err = validate_collection_name("").unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidCollectionName { ref name, .. } if name.is_empty()
        ));
    }

    #[test]
    fn validate_collection_name_rejects_too_long() {
        let too_long = "a".repeat(65);
        let err = validate_collection_name(&too_long).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidCollectionName { .. }));
    }

    #[test]
    fn validate_collection_name_rejects_sql_metacharacters() {
        assert!(validate_collection_name("foo'bar").is_err());
        // `-` is a member of the accepted charset; `--` inside the name is
        // still safe because the charset bans every quote / semicolon /
        // whitespace that DataFusion's SQL grammar would treat as a token
        // boundary, so the name can never escape its single-quoted literal.
        assert!(validate_collection_name("foo--bar").is_ok());
        assert!(validate_collection_name("foo bar").is_err()); // space rejected
        assert!(validate_collection_name("foo/bar").is_err()); // slash rejected
        assert!(validate_collection_name("foo;DROP").is_err());
        assert!(validate_collection_name("日本語").is_err()); // non-ASCII rejected
    }

    #[test]
    fn embed_parallelism_capped_clamps_at_max_when_user_exceeds_ceiling() {
        let cfg = RuntimeConfig {
            memory_limit_mb: 8192,
            embed_parallelism: MAX_EMBED_PARALLELISM + 1,
        };
        assert_eq!(cfg.embed_parallelism_capped(), MAX_EMBED_PARALLELISM);
    }

    #[test]
    fn embed_parallelism_capped_returns_user_value_when_within_ceiling() {
        let cfg = RuntimeConfig {
            memory_limit_mb: 8192,
            embed_parallelism: 8,
        };
        // Within the sanity ceiling ⇒ the user's value is returned
        // verbatim. The watchdog (`memory_limit_mb`) still enforces the
        // memory budget separately.
        assert_eq!(cfg.embed_parallelism_capped(), 8);
    }

    #[test]
    fn embed_parallelism_capped_preserves_explicit_sequential_choice() {
        let cfg = RuntimeConfig {
            memory_limit_mb: 8192,
            embed_parallelism: 0,
        };
        // The user explicitly opted into the sequential path; the cap
        // must not promote it back to a parallel run.
        assert_eq!(cfg.embed_parallelism_capped(), 0);
    }

    #[test]
    fn embed_parallelism_capped_is_independent_of_memory_limit_mb() {
        // Same `embed_parallelism`, opposite extremes of
        // `memory_limit_mb`: the cap must produce identical output.
        // Anything else would mean the user's value silently shifts
        // when another config field changes — which is the failure
        // mode the fixed-ceiling design exists to avoid.
        let cfg_tight = RuntimeConfig {
            memory_limit_mb: 100,
            embed_parallelism: 8,
        };
        let cfg_loose = RuntimeConfig {
            memory_limit_mb: u64::MAX,
            embed_parallelism: 8,
        };
        assert_eq!(cfg_tight.embed_parallelism_capped(), 8);
        assert_eq!(cfg_loose.embed_parallelism_capped(), 8);
    }

    #[test]
    fn init_template_omits_ollama_section_when_endpoint_is_default() {
        let cfg = Config::init_template();
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            !yaml.contains("ollama"),
            "init template must not surface the ollama subsection when it \
             carries the default loopback endpoint; got:\n{yaml}"
        );
    }

    #[test]
    fn legacy_yaml_without_ollama_key_deserializes_to_default_endpoint() {
        // Predates the ollama subsection: makes sure existing
        // `config.yml` files round-trip without touching the new field.
        let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 8
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.embedding.ollama.endpoint, DEFAULT_OLLAMA_ENDPOINT);
    }

    #[test]
    fn explicit_ollama_endpoint_override_round_trips() {
        let mut cfg = Config::init_template();
        cfg.embedding.ollama.endpoint = "http://gpu.lan:11434".to_string();
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let back: Config = serde_yaml_ng::from_str(&yaml).expect("deserialize");
        assert_eq!(back.embedding.ollama.endpoint, "http://gpu.lan:11434");
    }

    #[test]
    fn non_default_ollama_endpoint_surfaces_in_serialized_yaml() {
        let mut cfg = Config::init_template();
        cfg.embedding.ollama.endpoint = "http://gpu.lan:11434".to_string();
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            yaml.contains("ollama"),
            "non-default endpoint must surface the ollama subsection; got:\n{yaml}"
        );
        assert!(
            yaml.contains("gpu.lan"),
            "non-default endpoint must appear in serialized YAML; got:\n{yaml}"
        );
    }
}
