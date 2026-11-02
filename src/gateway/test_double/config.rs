//! A test double for [`Config`].

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;

use crate::config::{Config, ConfigError, SettingChange, Settings};

type CurrentFn = dyn Fn() -> Result<Settings, ConfigError> + Send + Sync;
type ChangeFn = dyn Fn(&ConfigCall) -> Result<SettingChange, ConfigError> + Send + Sync;

/// Answers `current` with [`TestConfig::default_settings`] unless replaced,
/// and `set` and `reset` from one handler, which panics unless replaced.
/// Records every call.
pub(crate) struct TestConfig {
    current: Box<CurrentFn>,
    change: Option<Box<ChangeFn>>,
    calls: Mutex<Vec<ConfigCall>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigCall {
    Current,
    Set { name: String, value: String },
    Reset { name: String },
}

impl TestConfig {
    pub(crate) fn new() -> Self {
        Self {
            current: Box::new(|| Ok(Self::default_settings())),
            change: None,
            calls: Mutex::default(),
        }
    }

    /// The settings a user has not changed.
    pub(crate) fn default_settings() -> Settings {
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

    pub(crate) fn on_current(
        mut self,
        f: impl Fn() -> Result<Settings, ConfigError> + Send + Sync + 'static,
    ) -> Self {
        self.current = Box::new(f);
        self
    }

    /// Answers both `set` and `reset`; the handler receives the call.
    pub(crate) fn on_change(
        mut self,
        f: impl Fn(&ConfigCall) -> Result<SettingChange, ConfigError> + Send + Sync + 'static,
    ) -> Self {
        self.change = Some(Box::new(f));
        self
    }

    pub(crate) fn calls(&self) -> Vec<ConfigCall> {
        self.calls.lock().unwrap().clone()
    }

    fn answer_change(&self, call: ConfigCall) -> Result<SettingChange, ConfigError> {
        self.calls.lock().unwrap().push(call.clone());
        let Some(change) = &self.change else {
            panic!("TestConfig has no handler for set and reset; set one with on_change");
        };
        change(&call)
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Config for TestConfig {
    async fn current(&self) -> Result<Settings, ConfigError> {
        self.calls.lock().unwrap().push(ConfigCall::Current);
        (self.current)()
    }

    async fn set(&self, name: &str, value: &str) -> Result<SettingChange, ConfigError> {
        self.answer_change(ConfigCall::Set {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }

    async fn reset(&self, name: &str) -> Result<SettingChange, ConfigError> {
        self.answer_change(ConfigCall::Reset {
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::config::SettingName;

    use super::*;

    #[tokio::test]
    async fn current_answers_the_defaults_unless_replaced() {
        let default = TestConfig::new();
        let replaced = TestConfig::new().on_current(|| {
            Err(ConfigError::CannotRead {
                source: "broken".into(),
            })
        });

        assert_eq!(
            default.current().await.unwrap(),
            TestConfig::default_settings()
        );
        assert!(replaced.current().await.is_err());
        assert_eq!(default.calls(), vec![ConfigCall::Current]);
    }

    #[tokio::test]
    async fn set_and_reset_go_to_the_change_handler_and_are_recorded() {
        let config = TestConfig::new().on_change(|call| {
            Ok(SettingChange {
                name: SettingName::McpShowVersion,
                before: serde_json::json!(true),
                after: serde_json::json!(matches!(call, ConfigCall::Set { .. })),
            })
        });

        let set = config.set("mcp.show_version", "false").await.unwrap();
        let reset = config.reset("mcp.show_version").await.unwrap();

        assert_eq!(set.after, serde_json::json!(true));
        assert_eq!(reset.after, serde_json::json!(false));
        assert_eq!(
            config.calls(),
            vec![
                ConfigCall::Set {
                    name: "mcp.show_version".to_owned(),
                    value: "false".to_owned()
                },
                ConfigCall::Reset {
                    name: "mcp.show_version".to_owned()
                },
            ]
        );
    }

    #[tokio::test]
    #[should_panic(expected = "no handler for set and reset")]
    async fn set_without_a_handler_panics() {
        let _ = TestConfig::new().set("mcp.show_version", "false").await;
    }
}
