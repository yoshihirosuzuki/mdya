//! YAML load/save for `~/.mdya/config.yml` via `serde_saphyr`. The parser
//! is a pure-Rust YAML 1.2 implementation built on `granit-parser` (a
//! fork of the original `saphyr-parser` project; the "saphyr" name in
//! the crate prefix is historical). The `unsafe-libyaml` C-port lineage
//! that `serde_yaml` / `serde_yaml_ng` carried is gone from the
//! mdya-direct dependency edge — it survives only as a transitive of
//! `lindera`'s own `serde_yaml` use, which is outside this module's
//! scope. The API surface we use (`from_str` / `to_string`) is the
//! standard serde data-format pair, so the swap is a one-symbol
//! migration on the call site.

use std::fs;
use std::path::Path;

use super::error::ConfigError;
use super::schema::Config;

/// Load `config.yml` from `path`. Returns the typed `Config` or a
/// `ConfigError` distinguishing IO failure, YAML parse failure, an
/// unsupported `embedding.model`, and an invalid collection name. Both
/// content checks run here so a user hand-editing `config.yml` to an
/// unrecognized model or a name the writer-layer SQL grammar cannot
/// represent is caught before any ingest touches the database.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let yaml = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    let config: Config =
        serde_saphyr::from_str(&yaml).map_err(|source| ConfigError::ParseYaml {
            path: path.to_owned(),
            source,
        })?;
    super::schema::validate_embedding_model(&config.embedding.model)?;
    for name in config.collections.keys() {
        super::schema::validate_collection_name(name)?;
    }
    Ok(config)
}

/// Serialize `config` to YAML and write atomically (write to `<path>.tmp`,
/// then rename) so a crash mid-write cannot leave the user with a truncated
/// `config.yml`.
pub fn save(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let yaml = serde_saphyr::to_string(config)?;
    let tmp = path.with_extension("yml.tmp");
    fs::write(&tmp, yaml).map_err(|source| ConfigError::Write {
        path: tmp.clone(),
        source,
    })?;
    if let Err(source) = fs::rename(&tmp, path) {
        // best-effort cleanup so a failed rename does not leave a `.tmp`
        // sibling next to the live `config.yml`.
        let _ = fs::remove_file(&tmp);
        return Err(ConfigError::Write {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::CollectionEntry;
    use tempfile::TempDir;

    #[test]
    fn save_then_load_yields_equal_config() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.yml");

        let mut original = Config::init_template();
        original.collections.insert(
            "notes".to_string(),
            CollectionEntry {
                path: "~/notes".to_string(),
                description: None,
            },
        );

        save(&path, &original).expect("save");
        let loaded = load(&path).expect("load");

        assert_eq!(loaded, original);
    }

    #[test]
    fn load_returns_parse_error_for_invalid_yaml() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("broken.yml");
        std::fs::write(&path, ": not yaml :").expect("write fixture");

        let err = load(&path).expect_err("must fail");
        assert!(matches!(err, ConfigError::ParseYaml { .. }));
    }

    #[test]
    fn load_rejects_a_hand_edited_unsupported_embedding_model() {
        // A user editing `embedding.model` to an unrecognized repo id must be
        // caught at load, not deferred to embedder construction.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.yml");
        std::fs::write(
            &path,
            "collections: {}\nembedding:\n  model: bogus/not-a-real-model\n",
        )
        .expect("write fixture");

        let err = load(&path).expect_err("must fail");
        assert!(matches!(
            err,
            ConfigError::UnsupportedEmbeddingModel { ref model, .. }
                if model == "bogus/not-a-real-model"
        ));
    }
}
