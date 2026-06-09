//! Idempotent bootstrap of the `<config_dir>/lance-models/lindera/ipadic/
//! config.yml` file that Lance's FTS tokenizer reads at `create_index`
//! time.
//!
//! Placement strategy: the IPADIC dictionary itself is embedded into the
//! mdya binary by the `lindera` crate's `ipadic` feature
//! (`include_bytes!` at build time), so the only thing mdya has to
//! write to disk is a ~100 byte `config.yml` whose
//! `segmenter.dictionary.kind: ipadic` directs Lance into the
//! `lindera_ipadic::ipadic::load()` path. Combined with the
//! `LANCE_LANGUAGE_MODEL_HOME` env redirect set by `main.rs`, this keeps
//! every file mdya writes inside `<config_dir>/lance-models/`, in the
//! single `~/.mdya/` namespace.
//!
//! Lives under `src/store/` rather than `src/config/` because the helper
//! is a LanceDB-engine concern (= what Lance needs on disk to load its
//! FTS tokenizer), not a YAML user-config concern. Both `cli::init` and
//! `ingest::writer` import it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Environment variable Lance consults to locate its language-model
/// directory tree (`lance-index-6.0.0/src/scalar/inverted/tokenizer.rs`).
/// `main.rs` redirects it to `<config_dir>/lance-models/` before the
/// tokio runtime starts so every subsequent FTS index touch reads from
/// inside `~/.mdya/`.
pub const LANCE_LANGUAGE_MODEL_HOME_ENV_KEY: &str = "LANCE_LANGUAGE_MODEL_HOME";

/// The tiny YAML that points Lance at the embedded IPADIC dictionary.
/// Trailing newline is intentional — keeps the file POSIX-tidy and
/// avoids editor diff noise.
pub const LINDERA_IPADIC_CONFIG_YML: &str =
    "segmenter:\n  mode: normal\n  dictionary:\n    kind: ipadic\n";

#[derive(Debug, Error)]
pub enum LanceLmError {
    #[error("create lance-models directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("read lindera/ipadic config.yml at {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("write lindera/ipadic config.yml at {path}: {source}")]
    WriteConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// `<config_dir>/lance-models/` — the directory that
/// `LANCE_LANGUAGE_MODEL_HOME` points at.
pub fn lance_models_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("lance-models")
}

/// `<config_dir>/lance-models/lindera/ipadic/config.yml`.
pub fn lindera_ipadic_config_path(config_dir: &Path) -> PathBuf {
    lance_models_dir(config_dir)
        .join("lindera")
        .join("ipadic")
        .join("config.yml")
}

/// Materialise the lindera/ipadic `config.yml` under `<config_dir>/
/// lance-models/`. Idempotent: if the file already exists with the
/// expected byte content, the call is a no-op and `mtime` is preserved.
/// If it is missing or has different content, write atomically via
/// `<...>.tmp` + `fs::rename` (same pattern as `src/config/store.rs`).
pub fn ensure_lindera_ipadic_config(config_dir: &Path) -> Result<(), LanceLmError> {
    let ipadic_dir = lance_models_dir(config_dir).join("lindera").join("ipadic");
    fs::create_dir_all(&ipadic_dir).map_err(|source| LanceLmError::Mkdir {
        path: ipadic_dir.clone(),
        source,
    })?;
    let config_path = ipadic_dir.join("config.yml");
    match fs::read(&config_path) {
        Ok(bytes) if bytes == LINDERA_IPADIC_CONFIG_YML.as_bytes() => Ok(()),
        Ok(_) => write_atomic(&config_path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => write_atomic(&config_path),
        Err(source) => Err(LanceLmError::ReadConfig {
            path: config_path,
            source,
        }),
    }
}

fn write_atomic(path: &Path) -> Result<(), LanceLmError> {
    let tmp = path.with_extension("yml.tmp");
    fs::write(&tmp, LINDERA_IPADIC_CONFIG_YML).map_err(|source| LanceLmError::WriteConfig {
        path: tmp.clone(),
        source,
    })?;
    if let Err(source) = fs::rename(&tmp, path) {
        // Best-effort cleanup so a failed rename does not leave a `.tmp`
        // sibling next to the canonical config.yml.
        let _ = fs::remove_file(&tmp);
        return Err(LanceLmError::WriteConfig {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_config_yml_on_first_call() {
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path();
        ensure_lindera_ipadic_config(base).expect("first call");
        let path = lindera_ipadic_config_path(base);
        assert!(
            path.is_file(),
            "config.yml not created at {}",
            path.display()
        );
        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, LINDERA_IPADIC_CONFIG_YML);
    }

    #[test]
    fn idempotent_when_existing_content_matches() {
        // Byte-equal short-circuit must preserve mtime; downstream
        // tools (incremental builds, file watchers) rely on it.
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path();
        ensure_lindera_ipadic_config(base).expect("first call");
        let path = lindera_ipadic_config_path(base);
        let first_mtime = fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime");

        // Cross the 1 s filesystem-mtime boundary so HFS+ (legacy macOS)
        // would have to bump mtime if we touched the file. APFS / ext4
        // resolve sub-second already so this only matters on legacy paths.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        ensure_lindera_ipadic_config(base).expect("second call");
        let second_mtime = fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime");
        assert_eq!(first_mtime, second_mtime);
    }

    #[test]
    fn rewrites_when_existing_content_differs() {
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path();
        let ipadic_dir = lance_models_dir(base).join("lindera").join("ipadic");
        fs::create_dir_all(&ipadic_dir).expect("mkdir");
        let path = ipadic_dir.join("config.yml");
        fs::write(&path, b"stale: true\n").expect("write stale");
        ensure_lindera_ipadic_config(base).expect("ensure");
        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, LINDERA_IPADIC_CONFIG_YML);
    }

    #[test]
    fn creates_missing_directory_tree() {
        // `<base>/lance-models/lindera/ipadic/` has zero ancestors —
        // helper must `create_dir_all` recursively rather than fail.
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path();
        assert!(!lance_models_dir(base).exists());
        ensure_lindera_ipadic_config(base).expect("ensure");
        assert!(lindera_ipadic_config_path(base).is_file());
    }
}
