//! Path resolution for `~/.mdya/` and `~/.mdya-models/`.
//!
//! Pure functions: the binary supplies the optional `--config-dir` /
//! `--model-cache-dir` values and we resolve to the final directory. The
//! only override path is the CLI flag (no env sniff happens here).

use std::path::{Path, PathBuf};

use super::error::ConfigError;

/// Resolve the mdya base directory.
///
/// Precedence: `--config-dir` flag > `$HOME/.mdya/`.
pub fn resolve_config_dir(flag: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(path) = flag {
        return Ok(expand_tilde(path));
    }
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".mdya"))
}

/// Resolve the embedding-model cache directory.
///
/// Precedence: `--model-cache-dir` flag > `$HOME/.mdya-models/`. Kept separate
/// from [`resolve_config_dir`] so a read-only `~/.mdya/` can be paired with a
/// writable model cache, and so two `--config-dir`s can share one downloaded
/// model.
pub fn resolve_model_cache_dir(flag: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(path) = flag {
        return Ok(expand_tilde(path));
    }
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".mdya-models"))
}

/// Expand a leading `~/` to the current user's home directory. No-op for
/// absolute paths or non-`~` prefixes; this uses `shellexpand`-style
/// tilde-only expansion.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_owned();
    };
    let expanded = shellexpand::tilde(s);
    PathBuf::from(expanded.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_with_absolute_path_is_returned_verbatim() {
        let p = PathBuf::from("/tmp/mdya-test");
        let resolved = resolve_config_dir(Some(&p)).expect("resolve");
        assert_eq!(resolved, p);
    }

    #[test]
    fn expand_tilde_replaces_leading_tilde_slash() {
        let home = dirs::home_dir().expect("home in test environment");
        let expanded = expand_tilde(Path::new("~/foo"));
        assert_eq!(expanded, home.join("foo"));
    }

    #[test]
    fn expand_tilde_leaves_absolute_path_unchanged() {
        let p = Path::new("/etc/hosts");
        assert_eq!(expand_tilde(p), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn model_cache_flag_with_absolute_path_is_returned_verbatim() {
        let p = PathBuf::from("/tmp/mdya-models-test");
        let resolved = resolve_model_cache_dir(Some(&p)).expect("resolve");
        assert_eq!(resolved, p);
    }

    #[test]
    fn model_cache_flag_expands_leading_tilde() {
        let home = dirs::home_dir().expect("home in test environment");
        let resolved =
            resolve_model_cache_dir(Some(Path::new("~/custom-models"))).expect("resolve");
        assert_eq!(resolved, home.join("custom-models"));
    }

    #[test]
    fn model_cache_default_is_home_mdya_models() {
        let home = dirs::home_dir().expect("home in test environment");
        let resolved = resolve_model_cache_dir(None).expect("resolve");
        assert_eq!(resolved, home.join(".mdya-models"));
    }
}
