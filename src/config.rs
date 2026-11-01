//! The `config` component: reads and changes the settings in the data
//! directory.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::BoxError;

/// Creates the component over the data directory. Nothing is read until a
/// function first needs it.
pub fn create(data_dir: PathBuf) -> Arc<dyn Config> {
    let _ = data_dir;
    todo!()
}

/// The name of a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingName {
    McpShowVersion,
    EmbeddingModel,
    EmbeddingOllamaEndpoint,
    ResourcesMemoryLimitMb,
    ResourcesEmbeddingParallelism,
    GetCliLimitBytes,
    GetMcpLimitBytes,
}

impl SettingName {
    /// The name the user writes (`mcp.show_version` and so on).
    pub fn as_str(self) -> &'static str {
        todo!()
    }
}

/// The current settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub mcp_show_version: bool,
    /// Only a value that names a supported embedding model.
    pub embedding_model: String,
    pub embedding_ollama_endpoint: String,
    /// 0 turns the memory watchdog off.
    pub resources_memory_limit_mb: u64,
    /// The value as set, including 0 and values above 1024.
    pub resources_embedding_parallelism: u64,
    /// 0 means no limit.
    pub get_cli_limit_bytes: u64,
    /// 0 means no limit.
    pub get_mcp_limit_bytes: u64,
}

impl Settings {
    /// How many documents to process at once (1 to 1024).
    pub fn parallelism(&self) -> usize {
        todo!()
    }

    /// The setting names and current values, in the order the settings
    /// operation shows them.
    pub fn entries(&self) -> Vec<(SettingName, serde_json::Value)> {
        todo!()
    }
}

/// A change made by `set` or `reset`.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingChange {
    pub name: SettingName,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `name` is the name the user gave.
    #[error("there is no setting named {name}")]
    UnknownName { name: String },
    /// `accepted` describes the values the setting accepts.
    #[error("the value is not valid for {name:?}")]
    InvalidValue { name: SettingName, accepted: String },
    #[error("the settings cannot be read")]
    CannotRead { source: BoxError },
    #[error("the settings cannot be written")]
    CannotWrite { source: BoxError },
}

#[async_trait]
pub trait Config: Send + Sync {
    async fn current(&self) -> Result<Settings, ConfigError>;
    async fn set(&self, name: &str, value: &str) -> Result<SettingChange, ConfigError>;
    async fn reset(&self, name: &str) -> Result<SettingChange, ConfigError>;
}

/// Checks a setting name and value without reading the settings. `value` is
/// `None` when resetting to the default. Callable without creating the
/// component.
pub fn check_setting(name: &str, value: Option<&str>) -> Result<SettingName, ConfigError> {
    let _ = (name, value);
    todo!()
}
