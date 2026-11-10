//! Tests of the component as `gateway` uses it, over real files in a temporary
//! directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use super::*;

fn component_over(directory: &Path) -> Arc<dyn Config> {
    create(directory.to_path_buf())
}

fn settings_file(directory: &Path) -> PathBuf {
    directory.join("config.json")
}

fn write_broken_settings(directory: &Path) {
    std::fs::write(settings_file(directory), b"not json").unwrap();
}

async fn default_settings() -> Settings {
    let directory = tempfile::tempdir().unwrap();
    component_over(directory.path()).current().await.unwrap()
}

fn assert_cannot_read(error: &ConfigError) {
    assert!(matches!(error, ConfigError::CannotRead { .. }), "{error:?}");
    assert!(std::error::Error::source(error).is_some(), "{error:?}");
}

#[test]
fn creating_the_component_does_not_touch_the_directory() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");

    let _component = component_over(&missing);

    assert!(!missing.exists());
}

#[tokio::test]
async fn current_gives_the_defaults_when_the_directory_is_not_there() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");

    let settings = component_over(&missing).current().await.unwrap();

    assert_eq!(settings, default_settings().await);
    assert!(!missing.exists());
}

#[tokio::test]
async fn current_gives_the_value_that_was_set() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());

    component
        .set("resources.memory_limit_mb", "42")
        .await
        .unwrap();

    let settings = component.current().await.unwrap();
    assert_eq!(settings.resources_memory_limit_mb, 42);
}

#[tokio::test]
async fn a_change_is_seen_by_a_component_created_later() {
    let directory = tempfile::tempdir().unwrap();
    component_over(directory.path())
        .set("mcp.show_version", "false")
        .await
        .unwrap();

    let settings = component_over(directory.path()).current().await.unwrap();

    assert!(!settings.mcp_show_version);
}

#[tokio::test]
async fn current_reports_a_broken_settings_file_instead_of_using_the_defaults() {
    let directory = tempfile::tempdir().unwrap();
    write_broken_settings(directory.path());

    let error = component_over(directory.path())
        .current()
        .await
        .unwrap_err();

    assert_cannot_read(&error);
}

#[tokio::test]
async fn current_reports_a_stored_value_that_the_setting_does_not_accept() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        settings_file(directory.path()),
        br#"{"embedding.model": "no/such-model"}"#,
    )
    .unwrap();

    let error = component_over(directory.path())
        .current()
        .await
        .unwrap_err();

    assert_cannot_read(&error);
}

#[tokio::test]
async fn current_reports_a_settings_file_that_cannot_be_opened() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(settings_file(directory.path())).unwrap();

    let error = component_over(directory.path())
        .current()
        .await
        .unwrap_err();

    assert_cannot_read(&error);
}

#[tokio::test]
async fn set_returns_the_name_and_the_values_before_and_after() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());

    let cases = [
        ("mcp.show_version", "false", json!(true), json!(false)),
        (
            "embedding.model",
            "ollama:nomic-embed-text",
            json!("cl-nagoya/ruri-v3-30m"),
            json!("ollama:nomic-embed-text"),
        ),
        ("get.cli_limit_bytes", "9", json!(1_048_576), json!(9)),
    ];
    for (name, value, before, after) in cases {
        let change = component.set(name, value).await.unwrap();

        assert_eq!(change.name.as_str(), name);
        assert_eq!(change.before, before, "{name}");
        assert_eq!(change.after, after, "{name}");
    }
}

#[tokio::test]
async fn set_returns_the_value_set_earlier_as_the_value_before() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    component.set("get.mcp_limit_bytes", "5").await.unwrap();

    let change = component.set("get.mcp_limit_bytes", "6").await.unwrap();

    assert_eq!(change.before, json!(5));
    assert_eq!(change.after, json!(6));
}

#[tokio::test]
async fn set_makes_the_data_directory_when_it_is_not_there() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("a").join("b");

    component_over(&missing)
        .set("mcp.show_version", "false")
        .await
        .unwrap();

    assert!(settings_file(&missing).is_file());
}

#[tokio::test]
async fn set_keeps_the_settings_set_earlier() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    component.set("get.cli_limit_bytes", "5").await.unwrap();

    component.set("get.mcp_limit_bytes", "6").await.unwrap();

    let settings = component.current().await.unwrap();
    assert_eq!(settings.get_cli_limit_bytes, 5);
    assert_eq!(settings.get_mcp_limit_bytes, 6);
}

#[tokio::test]
async fn set_rejects_an_unknown_name_without_writing() {
    let directory = tempfile::tempdir().unwrap();

    let error = component_over(directory.path())
        .set("no.such.setting", "1")
        .await
        .unwrap_err();

    assert!(
        matches!(&error, ConfigError::UnknownName { name } if name == "no.such.setting"),
        "{error:?}"
    );
    assert!(!settings_file(directory.path()).exists());
}

#[tokio::test]
async fn set_rejects_a_value_the_setting_does_not_accept_without_writing() {
    let directory = tempfile::tempdir().unwrap();

    let error = component_over(directory.path())
        .set("mcp.show_version", "yes")
        .await
        .unwrap_err();

    assert!(
        matches!(
            error,
            ConfigError::InvalidValue {
                name: SettingName::McpShowVersion,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!settings_file(directory.path()).exists());
}

// The name and the value need no file to check, so they are checked first.
#[tokio::test]
async fn set_reports_an_invalid_value_before_a_broken_settings_file() {
    let directory = tempfile::tempdir().unwrap();
    write_broken_settings(directory.path());

    let error = component_over(directory.path())
        .set("mcp.show_version", "yes")
        .await
        .unwrap_err();

    assert!(
        matches!(error, ConfigError::InvalidValue { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn set_leaves_a_broken_settings_file_as_it_is() {
    let directory = tempfile::tempdir().unwrap();
    write_broken_settings(directory.path());

    let error = component_over(directory.path())
        .set("mcp.show_version", "false")
        .await
        .unwrap_err();

    assert_cannot_read(&error);
    assert_eq!(
        std::fs::read(settings_file(directory.path())).unwrap(),
        b"not json"
    );
}

// Under a dangling symbolic link, reading finds no file and making the
// directory fails, whichever user runs the test. Not on Windows, where making
// a symbolic link needs a privilege.
#[cfg(unix)]
#[tokio::test]
async fn set_and_reset_report_a_directory_that_cannot_be_made() {
    let directory = tempfile::tempdir().unwrap();
    let dangling = directory.path().join("dangling");
    std::os::unix::fs::symlink(directory.path().join("nowhere"), &dangling).unwrap();
    let component = component_over(&dangling.join("data"));

    let set_error = component.set("mcp.show_version", "false").await;
    let reset_error = component.reset("mcp.show_version").await;

    for error in [set_error.unwrap_err(), reset_error.unwrap_err()] {
        assert!(
            matches!(error, ConfigError::CannotWrite { .. }),
            "{error:?}"
        );
        assert!(std::error::Error::source(&error).is_some(), "{error:?}");
    }
}

#[tokio::test]
async fn reset_returns_the_name_and_the_values_before_and_after() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    component.set("get.cli_limit_bytes", "5").await.unwrap();

    let change = component.reset("get.cli_limit_bytes").await.unwrap();

    assert_eq!(change.name, SettingName::GetCliLimitBytes);
    assert_eq!(change.before, json!(5));
    assert_eq!(change.after, json!(1_048_576));
}

#[tokio::test]
async fn reset_brings_back_the_default_and_keeps_the_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    component.set("get.cli_limit_bytes", "5").await.unwrap();
    component.set("get.mcp_limit_bytes", "6").await.unwrap();

    component.reset("get.cli_limit_bytes").await.unwrap();

    let settings = component.current().await.unwrap();
    assert_eq!(settings.get_cli_limit_bytes, 1_048_576);
    assert_eq!(settings.get_mcp_limit_bytes, 6);
}

#[tokio::test]
async fn reset_of_a_setting_never_set_returns_its_default_twice() {
    let directory = tempfile::tempdir().unwrap();

    let change = component_over(directory.path())
        .reset("mcp.show_version")
        .await
        .unwrap();

    assert_eq!(change.before, json!(true));
    assert_eq!(change.after, json!(true));
}

#[tokio::test]
async fn reset_rejects_an_unknown_name() {
    let directory = tempfile::tempdir().unwrap();

    let error = component_over(directory.path())
        .reset("no.such.setting")
        .await
        .unwrap_err();

    assert!(
        matches!(error, ConfigError::UnknownName { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn reset_leaves_a_broken_settings_file_as_it_is() {
    let directory = tempfile::tempdir().unwrap();
    write_broken_settings(directory.path());

    let error = component_over(directory.path())
        .reset("mcp.show_version")
        .await
        .unwrap_err();

    assert_cannot_read(&error);
    assert_eq!(
        std::fs::read(settings_file(directory.path())).unwrap(),
        b"not json"
    );
}

// Each change reads the file, changes one setting, and writes it back, so a
// change made while another is between its read and its write would be lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_made_at_the_same_time_all_take_effect() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    let names = [
        "resources.memory_limit_mb",
        "resources.embedding_parallelism",
        "get.cli_limit_bytes",
        "get.mcp_limit_bytes",
    ];

    let mut changes = tokio::task::JoinSet::new();
    for name in names {
        let component = Arc::clone(&component);
        changes.spawn(async move { component.set(name, "77").await });
    }
    while let Some(change) = changes.join_next().await {
        change.unwrap().unwrap();
    }

    let settings = component.current().await.unwrap();
    assert_eq!(settings.resources_memory_limit_mb, 77);
    assert_eq!(settings.resources_embedding_parallelism, 77);
    assert_eq!(settings.get_cli_limit_bytes, 77);
    assert_eq!(settings.get_mcp_limit_bytes, 77);
}

// Not on Windows, where replacing a file that a reader has open may be refused.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_during_a_change_sees_the_settings_from_before_or_after_it() {
    let directory = tempfile::tempdir().unwrap();
    let component = component_over(directory.path());
    component.set("get.cli_limit_bytes", "1").await.unwrap();

    let writer = {
        let component = Arc::clone(&component);
        tokio::spawn(async move {
            for value in 0..20 {
                let value = (value % 2 + 1).to_string();
                component.set("get.cli_limit_bytes", &value).await.unwrap();
            }
        })
    };
    while !writer.is_finished() {
        let limit = component.current().await.unwrap().get_cli_limit_bytes;
        assert!(limit == 1 || limit == 2, "{limit}");
    }
    writer.await.unwrap();
}
