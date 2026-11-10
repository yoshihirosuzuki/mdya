//! The settings the user has decided, in the form the settings file holds them.

use serde_json::{Map, Value};

use super::{MAX_NUMBER, SettingName, Settings, ValueKind};

/// The settings file's content. A setting the user has not decided is not in
/// the file and has its default value.
#[derive(Debug)]
pub(super) struct StoredSettings {
    settings: Settings,
    decided: Vec<SettingName>,
}

/// Why the content of the settings file cannot be used.
#[derive(Debug, thiserror::Error)]
pub(super) enum UnusableContent {
    #[error("the settings file is not valid JSON")]
    NotJson(#[source] serde_json::Error),
    #[error("the settings file does not hold a JSON object")]
    NotAnObject,
    #[error("the settings file has a setting that does not exist: {name}")]
    UnknownName { name: String },
    #[error("the settings file has a value that {name} does not accept")]
    InvalidValue { name: &'static str },
}

impl StoredSettings {
    pub(super) fn nothing_decided() -> Self {
        Self {
            settings: default_settings(),
            decided: Vec::new(),
        }
    }

    pub(super) fn parse(content: &[u8]) -> Result<Self, UnusableContent> {
        // Parsing into a `Value` first, because parsing straight into a map
        // would take `null` for an empty map.
        let parsed = serde_json::from_slice(content).map_err(UnusableContent::NotJson)?;
        let Value::Object(entries) = parsed else {
            return Err(UnusableContent::NotAnObject);
        };
        let mut stored = Self::nothing_decided();
        for (name, value) in &entries {
            stored.take_entry(name, value)?;
        }
        Ok(stored)
    }

    fn take_entry(&mut self, name: &str, value: &Value) -> Result<(), UnusableContent> {
        let setting = SettingName::from_name(name).ok_or_else(|| UnusableContent::UnknownName {
            name: name.to_owned(),
        })?;
        assign(&mut self.settings, setting, value).ok_or(UnusableContent::InvalidValue {
            name: setting.as_str(),
        })?;
        self.mark_decided(setting);
        Ok(())
    }

    /// `value` must be one that `check_setting` accepted for `setting`.
    #[expect(clippy::expect_used)]
    pub(super) fn set(&mut self, setting: SettingName, value: &str) {
        assign(&mut self.settings, setting, &json_value(setting, value))
            .expect("what check_setting accepts for a setting is what the setting can hold");
        self.mark_decided(setting);
    }

    #[expect(clippy::expect_used)]
    pub(super) fn reset(&mut self, setting: SettingName) {
        assign(
            &mut self.settings,
            setting,
            &default_settings().value_of(setting),
        )
        .expect("a default value is one its setting can hold");
        self.decided.retain(|decided| *decided != setting);
    }

    fn mark_decided(&mut self, setting: SettingName) {
        if !self.decided.contains(&setting) {
            self.decided.push(setting);
        }
    }

    pub(super) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(super) fn into_settings(self) -> Settings {
        self.settings
    }

    pub(super) fn to_file_content(&self) -> Vec<u8> {
        let entries: Map<String, Value> = self
            .decided
            .iter()
            .map(|setting| {
                (
                    setting.as_str().to_owned(),
                    self.settings.value_of(*setting),
                )
            })
            .collect();
        format!("{:#}\n", Value::Object(entries)).into_bytes()
    }
}

fn default_settings() -> Settings {
    Settings {
        mcp_show_version: true,
        embedding_model: "cl-nagoya/ruri-v3-30m".to_owned(),
        embedding_ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
        resources_memory_limit_mb: 8192,
        resources_embedding_parallelism: 8,
        get_cli_limit_bytes: 1_048_576,
        get_mcp_limit_bytes: 1_048_576,
    }
}

/// Puts `value` in `settings`, or returns `None` when `setting` does not accept
/// it.
fn assign(settings: &mut Settings, setting: SettingName, value: &Value) -> Option<()> {
    match setting {
        SettingName::McpShowVersion => settings.mcp_show_version = value.as_bool()?,
        SettingName::EmbeddingModel => settings.embedding_model = accepted_text(setting, value)?,
        SettingName::EmbeddingOllamaEndpoint => {
            settings.embedding_ollama_endpoint = accepted_text(setting, value)?;
        }
        SettingName::ResourcesMemoryLimitMb => {
            settings.resources_memory_limit_mb = accepted_number(value)?;
        }
        SettingName::ResourcesEmbeddingParallelism => {
            settings.resources_embedding_parallelism = accepted_number(value)?;
        }
        SettingName::GetCliLimitBytes => settings.get_cli_limit_bytes = accepted_number(value)?,
        SettingName::GetMcpLimitBytes => settings.get_mcp_limit_bytes = accepted_number(value)?,
    }
    Some(())
}

fn accepted_text(setting: SettingName, value: &Value) -> Option<String> {
    let text = value.as_str()?;
    setting.check_value(text).ok()?;
    Some(text.to_owned())
}

fn accepted_number(value: &Value) -> Option<u64> {
    value.as_u64().filter(|number| *number <= MAX_NUMBER)
}

/// The JSON form of `value`, which `check_setting` accepted for `setting`.
#[expect(clippy::expect_used)]
fn json_value(setting: SettingName, value: &str) -> Value {
    match setting.value_kind() {
        ValueKind::TrueOrFalse => Value::Bool(value == "true"),
        ValueKind::Number => Value::from(
            value
                .parse::<u64>()
                .expect("check_setting accepts only digits up to MAX_NUMBER, which fits in u64"),
        ),
        ValueKind::EmbeddingModel | ValueKind::HttpUrl => Value::from(value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const NUMBER_SETTINGS: [SettingName; 4] = [
        SettingName::ResourcesMemoryLimitMb,
        SettingName::ResourcesEmbeddingParallelism,
        SettingName::GetCliLimitBytes,
        SettingName::GetMcpLimitBytes,
    ];

    fn parsed(content: Value) -> Result<StoredSettings, UnusableContent> {
        StoredSettings::parse(content.to_string().as_bytes())
    }

    fn stored_with(name: SettingName, value: Value) -> Result<StoredSettings, UnusableContent> {
        parsed(json!({ name.as_str(): value }))
    }

    fn file_json(stored: &StoredSettings) -> Value {
        serde_json::from_slice(&stored.to_file_content()).unwrap()
    }

    fn assert_invalid_value(name: SettingName, value: Value) {
        let error = stored_with(name, value.clone()).expect_err(&value.to_string());
        assert!(
            matches!(error, UnusableContent::InvalidValue { name: named } if named == name.as_str()),
            "{name:?} with {value}: {error:?}"
        );
    }

    #[test]
    fn nothing_decided_gives_the_default_of_every_setting() {
        assert_eq!(
            StoredSettings::nothing_decided().into_settings(),
            Settings {
                mcp_show_version: true,
                embedding_model: "cl-nagoya/ruri-v3-30m".to_owned(),
                embedding_ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
                resources_memory_limit_mb: 8192,
                resources_embedding_parallelism: 8,
                get_cli_limit_bytes: 1_048_576,
                get_mcp_limit_bytes: 1_048_576,
            }
        );
    }

    #[test]
    fn an_empty_object_decides_nothing() {
        assert_eq!(
            parsed(json!({})).unwrap().into_settings(),
            StoredSettings::nothing_decided().into_settings()
        );
    }

    #[test]
    fn parse_takes_the_value_of_every_setting_from_the_content() {
        let stored = parsed(json!({
            "mcp.show_version": false,
            "embedding.model": "ollama:nomic-embed-text",
            "embedding.ollama_endpoint": "https://ollama.example:8443",
            "resources.memory_limit_mb": 1,
            "resources.embedding_parallelism": 2,
            "get.cli_limit_bytes": 3,
            "get.mcp_limit_bytes": 4,
        }))
        .unwrap();

        assert_eq!(
            stored.into_settings(),
            Settings {
                mcp_show_version: false,
                embedding_model: "ollama:nomic-embed-text".to_owned(),
                embedding_ollama_endpoint: "https://ollama.example:8443".to_owned(),
                resources_memory_limit_mb: 1,
                resources_embedding_parallelism: 2,
                get_cli_limit_bytes: 3,
                get_mcp_limit_bytes: 4,
            }
        );
    }

    #[test]
    fn parse_leaves_a_setting_not_in_the_content_at_its_default() {
        let settings = stored_with(SettingName::GetCliLimitBytes, json!(5))
            .unwrap()
            .into_settings();

        assert_eq!(settings.get_cli_limit_bytes, 5);
        assert_eq!(settings.get_mcp_limit_bytes, 1_048_576);
    }

    #[test]
    fn parse_accepts_both_ends_of_the_number_range() {
        for name in NUMBER_SETTINGS {
            for number in [0_u64, 9_007_199_254_740_991] {
                let stored = stored_with(name, json!(number)).unwrap();
                assert_eq!(stored.settings().value_of(name), json!(number));
            }
        }
    }

    #[test]
    fn parse_rejects_content_that_is_not_json() {
        for content in [&b""[..], b"{", b"not json", &[0xff, 0xfe]] {
            let error = StoredSettings::parse(content).unwrap_err();
            assert!(matches!(error, UnusableContent::NotJson(_)), "{error:?}");
        }
    }

    #[test]
    fn parse_rejects_json_that_is_not_an_object() {
        for content in [json!(null), json!([]), json!(1), json!("text"), json!(true)] {
            let error = parsed(content.clone()).unwrap_err();
            assert!(
                matches!(error, UnusableContent::NotAnObject),
                "{content}: {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_a_name_that_is_not_written_exactly_as_a_setting_is() {
        for name in ["no.such.setting", "MCP.show_version", " mcp.show_version"] {
            let error = parsed(json!({ name: true })).unwrap_err();
            assert!(
                matches!(&error, UnusableContent::UnknownName { name: named } if named == name),
                "{name}: {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_anything_but_a_json_boolean_for_the_version_setting() {
        for value in [json!("true"), json!(1), json!(null), json!([true])] {
            assert_invalid_value(SettingName::McpShowVersion, value);
        }
    }

    #[test]
    fn parse_rejects_anything_but_a_json_integer_in_range_for_a_number_setting() {
        for name in NUMBER_SETTINGS {
            for value in [
                json!("8"),
                json!(true),
                json!(null),
                json!(-1),
                json!(1.5),
                json!(9_007_199_254_740_992_u64),
            ] {
                assert_invalid_value(name, value);
            }
        }
    }

    #[test]
    fn parse_rejects_a_model_that_embedding_does_not_support() {
        for value in [json!("no/such-model"), json!("ollama:"), json!(1)] {
            assert_invalid_value(SettingName::EmbeddingModel, value);
        }
    }

    #[test]
    fn parse_rejects_an_endpoint_that_is_not_an_http_url() {
        for value in [json!("ftp://example"), json!("127.0.0.1:11434"), json!(1)] {
            assert_invalid_value(SettingName::EmbeddingOllamaEndpoint, value);
        }
    }

    #[test]
    fn set_writes_true_or_false_as_a_json_boolean() {
        let mut stored = StoredSettings::nothing_decided();

        stored.set(SettingName::McpShowVersion, "false");

        assert!(!stored.settings().mcp_show_version);
        assert_eq!(file_json(&stored), json!({ "mcp.show_version": false }));
    }

    #[test]
    fn set_writes_digits_as_a_json_number_without_their_leading_zeros() {
        for name in NUMBER_SETTINGS {
            let mut stored = StoredSettings::nothing_decided();

            stored.set(name, "0008");

            assert_eq!(stored.settings().value_of(name), json!(8));
            assert_eq!(file_json(&stored), json!({ name.as_str(): 8 }));
        }
    }

    #[test]
    fn set_writes_a_model_and_an_endpoint_as_json_strings() {
        let mut stored = StoredSettings::nothing_decided();

        stored.set(SettingName::EmbeddingModel, "ollama:nomic-embed-text");
        stored.set(SettingName::EmbeddingOllamaEndpoint, "https://host:1");

        assert_eq!(
            file_json(&stored),
            json!({
                "embedding.model": "ollama:nomic-embed-text",
                "embedding.ollama_endpoint": "https://host:1",
            })
        );
    }

    #[test]
    fn set_leaves_the_other_settings_as_they_were() {
        let mut stored = stored_with(SettingName::GetCliLimitBytes, json!(5)).unwrap();

        stored.set(SettingName::GetMcpLimitBytes, "6");

        assert_eq!(
            file_json(&stored),
            json!({ "get.cli_limit_bytes": 5, "get.mcp_limit_bytes": 6 })
        );
    }

    #[test]
    fn setting_a_setting_twice_keeps_the_last_value() {
        let mut stored = StoredSettings::nothing_decided();

        stored.set(SettingName::GetCliLimitBytes, "5");
        stored.set(SettingName::GetCliLimitBytes, "6");

        assert_eq!(file_json(&stored), json!({ "get.cli_limit_bytes": 6 }));
    }

    #[test]
    fn reset_gives_every_setting_its_default_and_takes_it_out_of_the_file() {
        for name in SettingName::ALL {
            let default = StoredSettings::nothing_decided().settings().value_of(name);
            let mut stored = stored_with(
                name,
                match name {
                    SettingName::McpShowVersion => json!(false),
                    SettingName::EmbeddingModel => json!("ollama:other"),
                    SettingName::EmbeddingOllamaEndpoint => json!("https://other"),
                    _ => json!(1),
                },
            )
            .unwrap();

            stored.reset(name);

            assert_eq!(stored.settings().value_of(name), default, "{name:?}");
            assert_eq!(file_json(&stored), json!({}), "{name:?}");
        }
    }

    #[test]
    fn reset_leaves_the_other_settings_as_they_were() {
        let mut stored = parsed(json!({
            "get.cli_limit_bytes": 5,
            "get.mcp_limit_bytes": 6,
        }))
        .unwrap();

        stored.reset(SettingName::GetCliLimitBytes);

        assert_eq!(file_json(&stored), json!({ "get.mcp_limit_bytes": 6 }));
    }

    #[test]
    fn resetting_a_setting_that_was_not_decided_changes_nothing() {
        let mut stored = StoredSettings::nothing_decided();

        stored.reset(SettingName::McpShowVersion);

        assert_eq!(
            stored.into_settings(),
            StoredSettings::nothing_decided().into_settings()
        );
    }

    #[test]
    fn nothing_decided_is_written_as_an_empty_object() {
        assert_eq!(file_json(&StoredSettings::nothing_decided()), json!({}));
    }

    #[test]
    fn a_setting_decided_with_its_default_value_is_still_written() {
        let mut stored = StoredSettings::nothing_decided();

        stored.set(SettingName::GetCliLimitBytes, "1048576");

        assert_eq!(
            file_json(&stored),
            json!({ "get.cli_limit_bytes": 1_048_576 })
        );
    }

    #[test]
    fn the_written_content_reads_back_as_the_same_settings() {
        let mut stored = StoredSettings::nothing_decided();
        stored.set(SettingName::McpShowVersion, "false");
        stored.set(SettingName::EmbeddingModel, "ollama:nomic-embed-text");
        stored.set(SettingName::EmbeddingOllamaEndpoint, "https://host:1");
        stored.set(SettingName::ResourcesMemoryLimitMb, "0");
        stored.set(SettingName::ResourcesEmbeddingParallelism, "2000");
        stored.set(SettingName::GetCliLimitBytes, "9007199254740991");
        stored.set(SettingName::GetMcpLimitBytes, "7");

        let read_back = StoredSettings::parse(&stored.to_file_content()).unwrap();

        assert_eq!(read_back.into_settings(), stored.into_settings());
    }

    #[test]
    fn the_written_content_ends_with_a_newline() {
        let content = StoredSettings::nothing_decided().to_file_content();

        assert_eq!(content.last(), Some(&b'\n'));
    }
}
