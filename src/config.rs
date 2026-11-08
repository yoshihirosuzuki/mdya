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
    /// Every setting, in the order the settings operation shows them.
    const ALL: [Self; 7] = [
        Self::McpShowVersion,
        Self::EmbeddingModel,
        Self::EmbeddingOllamaEndpoint,
        Self::ResourcesMemoryLimitMb,
        Self::ResourcesEmbeddingParallelism,
        Self::GetCliLimitBytes,
        Self::GetMcpLimitBytes,
    ];

    /// The name the user writes (`mcp.show_version` and so on).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::McpShowVersion => "mcp.show_version",
            Self::EmbeddingModel => "embedding.model",
            Self::EmbeddingOllamaEndpoint => "embedding.ollama_endpoint",
            Self::ResourcesMemoryLimitMb => "resources.memory_limit_mb",
            Self::ResourcesEmbeddingParallelism => "resources.embedding_parallelism",
            Self::GetCliLimitBytes => "get.cli_limit_bytes",
            Self::GetMcpLimitBytes => "get.mcp_limit_bytes",
        }
    }

    /// The setting that `name` names, written exactly as `as_str` returns it.
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.as_str() == name)
    }

    fn value_kind(self) -> ValueKind {
        match self {
            Self::McpShowVersion => ValueKind::TrueOrFalse,
            Self::EmbeddingModel => ValueKind::EmbeddingModel,
            Self::EmbeddingOllamaEndpoint => ValueKind::HttpUrl,
            Self::ResourcesMemoryLimitMb
            | Self::ResourcesEmbeddingParallelism
            | Self::GetCliLimitBytes
            | Self::GetMcpLimitBytes => ValueKind::Number,
        }
    }

    /// Checks that `value` is one this setting accepts.
    fn check_value(self, value: &str) -> Result<(), ConfigError> {
        let kind = self.value_kind();
        if kind.accepts(value) {
            return Ok(());
        }
        Err(ConfigError::InvalidValue {
            name: self,
            accepted: kind.accepted().to_owned(),
        })
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

/// The most documents processed at once, however high the parallelism is set.
const MAX_PARALLELISM: usize = 1024;

impl Settings {
    /// How many documents to process at once (1 to 1024).
    pub fn parallelism(&self) -> usize {
        // A value too large for `usize` is above the maximum too.
        usize::try_from(self.resources_embedding_parallelism)
            .map_or(MAX_PARALLELISM, |set| set.clamp(1, MAX_PARALLELISM))
    }

    /// The setting names and current values, in the order the settings
    /// operation shows them.
    pub fn entries(&self) -> Vec<(SettingName, serde_json::Value)> {
        SettingName::ALL
            .into_iter()
            .map(|name| (name, self.value_of(name)))
            .collect()
    }

    fn value_of(&self, name: SettingName) -> serde_json::Value {
        match name {
            SettingName::McpShowVersion => self.mcp_show_version.into(),
            SettingName::EmbeddingModel => self.embedding_model.as_str().into(),
            SettingName::EmbeddingOllamaEndpoint => self.embedding_ollama_endpoint.as_str().into(),
            SettingName::ResourcesMemoryLimitMb => self.resources_memory_limit_mb.into(),
            SettingName::ResourcesEmbeddingParallelism => {
                self.resources_embedding_parallelism.into()
            }
            SettingName::GetCliLimitBytes => self.get_cli_limit_bytes.into(),
            SettingName::GetMcpLimitBytes => self.get_mcp_limit_bytes.into(),
        }
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
    let setting = SettingName::from_name(name).ok_or_else(|| ConfigError::UnknownName {
        name: name.to_owned(),
    })?;
    if let Some(value) = value {
        setting.check_value(value)?;
    }
    Ok(setting)
}

/// The largest value a number setting accepts. In its JSON output, the
/// settings operation writes these values as numbers, and JSON
/// implementations agree exactly on the value of an integer up to 2^53 − 1
/// (RFC 8259, section 6).
const MAX_NUMBER: u64 = 9_007_199_254_740_991;

/// The kinds of values the user writes when changing a setting.
#[derive(Clone, Copy)]
enum ValueKind {
    /// `true` or `false`.
    TrueOrFalse,
    /// An integer from 0 to [`MAX_NUMBER`], in decimal digits only. Leading
    /// zeros are allowed; a sign, white space, and other characters are not.
    Number,
    /// A value that `embedding` says names a supported model.
    EmbeddingModel,
    /// A URL that starts with `http://` or `https://` (in lowercase), with no
    /// white space or control characters anywhere.
    HttpUrl,
}

impl ValueKind {
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::TrueOrFalse => value == "true" || value == "false",
            Self::Number => is_decimal_number(value),
            Self::EmbeddingModel => crate::embedding::is_supported_model(value),
            Self::HttpUrl => is_http_url(value),
        }
    }

    /// What the setting accepts, for the error.
    fn accepted(self) -> &'static str {
        match self {
            Self::TrueOrFalse => "true or false",
            Self::Number => "an integer from 0 to 9007199254740991, in decimal digits",
            Self::EmbeddingModel => {
                "the Hugging Face repository of a supported model, or ollama: followed by a model name"
            }
            Self::HttpUrl => "a URL that starts with http:// or https://",
        }
    }
}

fn is_decimal_number(value: &str) -> bool {
    // `u64::from_str` also takes a leading `+`, so the digits are checked
    // first.
    let only_digits = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    only_digits
        && value
            .parse::<u64>()
            .is_ok_and(|number| number <= MAX_NUMBER)
}

fn is_http_url(value: &str) -> bool {
    // The URL parser forgives spaces at either end and drops a tab or newline
    // anywhere, but a value with them is not a URL as written: a trailing
    // space, for one, ends up inside the request URL once `embedding` appends
    // its path. So they are turned away here.
    let starts_with_scheme = value.starts_with("http://") || value.starts_with("https://");
    let has_no_spaces_or_controls = !value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control());
    starts_with_scheme && has_no_spaces_or_controls && reqwest::Url::parse(value).is_ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NAMES: [(SettingName, &str); 7] = [
        (SettingName::McpShowVersion, "mcp.show_version"),
        (SettingName::EmbeddingModel, "embedding.model"),
        (
            SettingName::EmbeddingOllamaEndpoint,
            "embedding.ollama_endpoint",
        ),
        (
            SettingName::ResourcesMemoryLimitMb,
            "resources.memory_limit_mb",
        ),
        (
            SettingName::ResourcesEmbeddingParallelism,
            "resources.embedding_parallelism",
        ),
        (SettingName::GetCliLimitBytes, "get.cli_limit_bytes"),
        (SettingName::GetMcpLimitBytes, "get.mcp_limit_bytes"),
    ];

    const NUMBER_SETTINGS: [SettingName; 4] = [
        SettingName::ResourcesMemoryLimitMb,
        SettingName::ResourcesEmbeddingParallelism,
        SettingName::GetCliLimitBytes,
        SettingName::GetMcpLimitBytes,
    ];

    fn settings() -> Settings {
        Settings {
            mcp_show_version: true,
            embedding_model: "cl-nagoya/ruri-v3-30m".to_owned(),
            embedding_ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
            resources_memory_limit_mb: 8192,
            resources_embedding_parallelism: 8,
            get_cli_limit_bytes: 1_048_576,
            get_mcp_limit_bytes: 0,
        }
    }

    fn with_parallelism(value: u64) -> Settings {
        Settings {
            resources_embedding_parallelism: value,
            ..settings()
        }
    }

    fn check_value(setting: SettingName, value: &str) -> Result<SettingName, ConfigError> {
        check_setting(setting.as_str(), Some(value))
    }

    fn assert_accepts(setting: SettingName, values: &[&str]) {
        for value in values {
            assert_eq!(
                check_value(setting, value).unwrap(),
                setting,
                "{setting:?} {value:?}"
            );
        }
    }

    fn assert_rejects(setting: SettingName, values: &[&str]) {
        for value in values {
            assert!(
                matches!(
                    check_value(setting, value),
                    Err(ConfigError::InvalidValue { name, .. }) if name == setting
                ),
                "{setting:?} {value:?}"
            );
        }
    }

    #[test]
    fn each_setting_is_named_as_the_user_writes_it() {
        for (setting, name) in NAMES {
            assert_eq!(setting.as_str(), name);
        }
    }

    #[test]
    fn a_parallelism_of_0_processes_one_document_at_a_time() {
        assert_eq!(with_parallelism(0).parallelism(), 1);
    }

    #[test]
    fn a_parallelism_from_1_to_1024_is_used_as_set() {
        for (value, documents) in [(1, 1), (8, 8), (1024, 1024)] {
            assert_eq!(with_parallelism(value).parallelism(), documents);
        }
    }

    #[test]
    fn a_parallelism_above_1024_is_taken_as_1024() {
        for value in [1025, u64::MAX] {
            assert_eq!(with_parallelism(value).parallelism(), 1024, "{value}");
        }
    }

    #[test]
    fn entries_shows_every_setting_with_its_value_in_the_listed_order() {
        assert_eq!(
            settings().entries(),
            vec![
                (SettingName::McpShowVersion, json!(true)),
                (SettingName::EmbeddingModel, json!("cl-nagoya/ruri-v3-30m")),
                (
                    SettingName::EmbeddingOllamaEndpoint,
                    json!("http://127.0.0.1:11434")
                ),
                (SettingName::ResourcesMemoryLimitMb, json!(8192)),
                (SettingName::ResourcesEmbeddingParallelism, json!(8)),
                (SettingName::GetCliLimitBytes, json!(1_048_576)),
                (SettingName::GetMcpLimitBytes, json!(0)),
            ]
        );
    }

    #[test]
    fn entries_shows_the_parallelism_as_set_not_as_used() {
        for value in [0, 2000] {
            assert!(
                with_parallelism(value)
                    .entries()
                    .contains(&(SettingName::ResourcesEmbeddingParallelism, json!(value))),
                "{value}"
            );
        }
    }

    #[test]
    fn check_setting_returns_the_setting_that_the_name_names() {
        for (setting, name) in NAMES {
            assert_eq!(check_setting(name, None).unwrap(), setting);
        }
    }

    #[test]
    fn check_setting_rejects_a_name_not_written_exactly_as_listed() {
        for name in [
            "",
            "mcp",
            "show_version",
            "MCP.show_version",
            " mcp.show_version",
            "mcp.show_version ",
        ] {
            assert!(
                matches!(
                    check_setting(name, None),
                    Err(ConfigError::UnknownName { name: given }) if given == name
                ),
                "{name:?}"
            );
        }
    }

    #[test]
    fn check_setting_reports_an_unknown_name_without_checking_the_value() {
        assert!(matches!(
            check_setting("no.such_setting", Some("not a value")),
            Err(ConfigError::UnknownName { .. })
        ));
    }

    #[test]
    fn the_version_setting_accepts_true_or_false() {
        assert_accepts(SettingName::McpShowVersion, &["true", "false"]);
    }

    #[test]
    fn the_version_setting_rejects_other_ways_of_writing_true_or_false() {
        assert_rejects(
            SettingName::McpShowVersion,
            &["", "True", "FALSE", "1", "yes", " true", "true "],
        );
    }

    #[test]
    fn number_settings_accept_decimal_integers_from_0_to_9007199254740991() {
        for setting in NUMBER_SETTINGS {
            assert_accepts(
                setting,
                &[
                    "0",
                    "8",
                    "1024",
                    "0008",
                    "9007199254740991",
                    "009007199254740991",
                ],
            );
        }
    }

    #[test]
    fn number_settings_reject_anything_but_decimal_digits_in_range() {
        for setting in NUMBER_SETTINGS {
            assert_rejects(
                setting,
                &[
                    "",
                    "-1",
                    "+8",
                    " 8",
                    "8 ",
                    "8.0",
                    "1e3",
                    "0x10",
                    "1_000",
                    "８",
                    "9007199254740992",
                    "009007199254740992",
                    "18446744073709551615",
                    "18446744073709551616",
                    "100000000000000000000",
                ],
            );
        }
    }

    #[test]
    fn the_embedding_model_setting_accepts_a_supported_model() {
        assert_accepts(
            SettingName::EmbeddingModel,
            &["cl-nagoya/ruri-v3-30m", "ollama:nomic-embed-text"],
        );
    }

    #[test]
    fn the_embedding_model_setting_rejects_a_model_it_does_not_support() {
        assert_rejects(
            SettingName::EmbeddingModel,
            &["", "ollama:", "example/unknown-model"],
        );
    }

    #[test]
    fn the_ollama_endpoint_setting_accepts_an_http_or_https_url() {
        assert_accepts(
            SettingName::EmbeddingOllamaEndpoint,
            &[
                "http://127.0.0.1:11434",
                "http://localhost:11434/",
                "http://[::1]:11434",
                "https://ollama.example.com/ollama",
            ],
        );
    }

    #[test]
    fn the_ollama_endpoint_setting_rejects_other_values() {
        assert_rejects(
            SettingName::EmbeddingOllamaEndpoint,
            &[
                "",
                "127.0.0.1:11434",
                "localhost:11434",
                "ftp://example.com",
                "HTTP://localhost:11434",
                "http://",
                "https://",
                " http://localhost:11434",
                "http://localhost:11434 ",
                "http://local host:11434",
                "http://localhost:99999",
            ],
        );
    }

    #[test]
    fn the_ollama_endpoint_setting_rejects_white_space_and_control_characters_anywhere() {
        // A URL parser alone takes each of these.
        assert_rejects(
            SettingName::EmbeddingOllamaEndpoint,
            &[
                "http://local\thost:11434",
                "http://localhost:11434\n",
                "http://localhost:11434/\u{1}",
                "http://localhost:11434/\u{3000}",
                "http://localhost:11434/\u{a0}",
                "http://localhost:11434/\u{9f}",
            ],
        );
    }

    #[test]
    fn a_rejected_value_comes_with_what_the_setting_accepts() {
        for (setting, value, expected) in [
            (SettingName::McpShowVersion, "yes", "true or false"),
            (
                SettingName::EmbeddingModel,
                "example/unknown-model",
                "the Hugging Face repository of a supported model, or ollama: followed by a model name",
            ),
            (
                SettingName::EmbeddingOllamaEndpoint,
                "localhost:11434",
                "a URL that starts with http:// or https://",
            ),
            (
                SettingName::ResourcesMemoryLimitMb,
                "-1",
                "an integer from 0 to 9007199254740991, in decimal digits",
            ),
        ] {
            assert!(
                matches!(
                    check_value(setting, value),
                    Err(ConfigError::InvalidValue { accepted, .. }) if accepted == expected
                ),
                "{setting:?}"
            );
        }
    }
}
