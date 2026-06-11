//! `mdya collection add <path>` implementation.
//!
//! Behavior:
//! - collection name: basename of the expanded path, overridable by
//!   `--name`.
//! - path validation: expand tilde, then assert existence + `is_dir` at
//!   this command's boundary. The `config.yml` stores the user-typed
//!   string verbatim so `~/notes` round-trips.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::info;

use crate::cli::OutputFormat;
use crate::config::{self, CollectionEntry, ConfigError, InvalidPathKind};
use crate::introspect::{self, output};

#[derive(Debug, Error)]
pub enum CollectionAddError {
    #[error(transparent)]
    Config(#[from] ConfigError),

}

pub fn run(
    config_dir_flag: Option<&Path>,
    path_arg: &Path,
    name_override: Option<&str>,
    description: Option<&str>,
) -> Result<(), CollectionAddError> {
    let base = config::resolve_config_dir(config_dir_flag)?;
    let config_path = base.join("config.yml");
    let mut cfg = config::load(&config_path)?;

    let expanded = validate_collection_path(path_arg)?;
    let name = resolve_collection_name(&expanded, name_override)?;
    config::validate_collection_name(&name)?;

    if cfg.collections.contains_key(&name) {
        return Err(ConfigError::CollectionExists { name }.into());
    }

    cfg.collections.insert(
        name.clone(),
        CollectionEntry {
            path: path_arg.display().to_string(),
            description: description.map(str::to_string),
        },
    );
    config::save(&config_path, &cfg)?;

    info!(name = %name, path = %expanded.display(), "collection added");
    Ok(())
}

/// `mdya collection list`: gather the registered collections (with document
/// counts) and render them in `format` to stdout.
pub async fn list(config_dir_flag: Option<&Path>, format: OutputFormat) -> anyhow::Result<()> {
    let report = introspect::collection_list(config_dir_flag).await?;
    let mut stdout = io::stdout().lock();
    match format {
        OutputFormat::Human => output::print_collections_human(&mut stdout, &report)?,
        OutputFormat::Json => output::print_collections_json(&mut stdout, &report)?,
        OutputFormat::Md => output::print_collections_md(&mut stdout, &report)?,
        OutputFormat::Xml => output::print_collections_xml(&mut stdout, &report)?,
    }
    Ok(())
}

fn validate_collection_path(path_arg: &Path) -> Result<PathBuf, ConfigError> {
    let expanded = config::expand_tilde(path_arg);
    if !expanded.exists() {
        return Err(ConfigError::InvalidCollectionPath {
            path: expanded,
            kind: InvalidPathKind::NotFound,
        });
    }
    if !expanded.is_dir() {
        return Err(ConfigError::InvalidCollectionPath {
            path: expanded,
            kind: InvalidPathKind::NotDirectory,
        });
    }
    Ok(expanded)
}

fn resolve_collection_name(
    expanded_path: &Path,
    name_override: Option<&str>,
) -> Result<String, CollectionAddError> {
    if let Some(n) = name_override {
        // Empty `--name` is rejected by `validate_collection_name` in the
        // caller, so it does not need its own variant here.
        return Ok(n.to_string());
    }
    expanded_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ConfigError::InvalidCollectionPath {
                path: expanded_path.to_path_buf(),
                kind: InvalidPathKind::NoBasename,
            }
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_dir(tmp: &TempDir) -> PathBuf {
        let base = tmp.path().to_path_buf();
        std::fs::create_dir_all(base.join("index")).expect("mkdir index");
        std::fs::create_dir_all(base.join("models")).expect("mkdir models");
        config::save(
            &base.join("config.yml"),
            &crate::config::Config::init_template(),
        )
        .expect("save config");
        base
    }

    #[test]
    fn add_with_basename_default_writes_entry() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let collection_path = tmp.path().join("notes");
        std::fs::create_dir(&collection_path).expect("mkdir notes");

        run(Some(&base), &collection_path, None, None).expect("add");

        let cfg = config::load(&base.join("config.yml")).expect("reload");
        assert_eq!(cfg.collections.len(), 1);
        assert!(cfg.collections.contains_key("notes"));
    }

    #[test]
    fn name_override_replaces_basename() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let collection_path = tmp.path().join("notes");
        std::fs::create_dir(&collection_path).expect("mkdir notes");

        run(Some(&base), &collection_path, Some("work-notes"), None).expect("add");

        let cfg = config::load(&base.join("config.yml")).expect("reload");
        assert!(cfg.collections.contains_key("work-notes"));
        assert!(!cfg.collections.contains_key("notes"));
    }

    #[test]
    fn add_with_description_stores_it() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let collection_path = tmp.path().join("notes");
        std::fs::create_dir(&collection_path).expect("mkdir notes");

        run(Some(&base), &collection_path, None, Some("個人メモ")).expect("add");

        let cfg = config::load(&base.join("config.yml")).expect("reload");
        assert_eq!(
            cfg.collections["notes"].description.as_deref(),
            Some("個人メモ")
        );
    }

    #[test]
    fn duplicate_name_returns_collection_exists() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let collection_path = tmp.path().join("notes");
        std::fs::create_dir(&collection_path).expect("mkdir notes");

        run(Some(&base), &collection_path, None, None).expect("first add");
        let err = run(Some(&base), &collection_path, None, None).expect_err("must fail");

        assert!(matches!(
            err,
            CollectionAddError::Config(ConfigError::CollectionExists { .. })
        ));
    }

    #[test]
    fn missing_path_returns_invalid_collection_path() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let missing = tmp.path().join("absent");

        let err = run(Some(&base), &missing, None, None).expect_err("must fail");
        assert!(matches!(
            err,
            CollectionAddError::Config(ConfigError::InvalidCollectionPath {
                kind: InvalidPathKind::NotFound,
                ..
            })
        ));
    }

    #[test]
    fn non_directory_path_returns_not_directory() {
        let tmp = TempDir::new().expect("tempdir");
        let base = init_dir(&tmp);
        let file_path = tmp.path().join("a.txt");
        std::fs::write(&file_path, b"hi").expect("write file");

        let err = run(Some(&base), &file_path, None, None).expect_err("must fail");
        assert!(matches!(
            err,
            CollectionAddError::Config(ConfigError::InvalidCollectionPath {
                kind: InvalidPathKind::NotDirectory,
                ..
            })
        ));
    }
}
