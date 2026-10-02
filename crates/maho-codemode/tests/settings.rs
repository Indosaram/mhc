use maho_codemode::config::settings::*;
use serde_json::json;
use std::path::Path;

async fn write_config(root: &Path, value: &str) {
    tokio::fs::create_dir_all(root.join(".maho")).await.expect("create test config directory");
    tokio::fs::write(root.join(".maho/codemode.json"), value).await.expect("write test config");
}

async fn loaded(value: serde_json::Value) -> LoadedCodemodeSettings {
    let root = tempfile::tempdir().expect("create temporary config root");
    write_config(root.path(), &value.to_string()).await;
    load_codemode_settings(root.path(), root.path()).await.expect("read test config")
}

#[tokio::test]
async fn defaults_without_file() {
    let root = tempfile::tempdir().unwrap();
    let settings = load_codemode_settings(root.path(), root.path()).await.unwrap();
    assert_eq!(settings.settings, CodemodeSettings::default());
    assert!(settings.source.is_none());
    assert!(settings.warnings.is_empty());
}

#[tokio::test]
async fn project_precedes_global() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let home = root.path().join("home");
    write_config(&project, r#"{"languages":{"py":false,"rb":true},"parallelPoolWidth":9,"maxDetachedCells":2}"#).await;
    tokio::fs::create_dir_all(home.join(".maho/agent")).await.unwrap();
    tokio::fs::write(home.join(".maho/agent/codemode.json"), r#"{"languages":{"js":false}}"#).await.unwrap();
    let result = load_codemode_settings(&project, &home).await.unwrap();
    assert_eq!(result.source, Some(project.join(".maho/codemode.json")));
    assert_eq!(result.settings.languages, Languages { py: false, js: true, rb: true, jl: false });
    assert!((result.settings.parallel_pool_width - 9.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn global_fallback() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join(".maho/agent")).await.unwrap();
    let file = root.path().join(".maho/agent/codemode.json");
    tokio::fs::write(&file, r#"{"languages":{"js":false,"jl":true},"cellTimeoutSeconds":12}"#).await.unwrap();
    let result = load_codemode_settings(&root.path().join("missing"), root.path()).await.unwrap();
    assert_eq!(result.source, Some(file));
    assert!(!result.settings.languages.js);
    assert!(result.settings.languages.jl);
    assert!((result.settings.cell_timeout_seconds - 12.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn partial_nested_defaults() {
    let result = loaded(json!({"taskTools":{"task":"custom"},"outputSink":{"maxColumns":0}})).await;
    assert_eq!(result.settings.task_tools.output, "task_output");
    assert_eq!(result.settings.task_tools.task, "custom");
    assert!((result.settings.output_sink.head_bytes - 20_480.0).abs() < f64::EPSILON);
    assert!(result.settings.output_sink.max_columns.abs() < f64::EPSILON);
}

#[tokio::test]
async fn status_defaults_and_override() {
    assert!(loaded(json!({})).await.settings.status_events);
    assert!(!loaded(json!({"statusEvents":false})).await.settings.status_events);
}

#[tokio::test]
async fn language_env_beats_file() {
    let settings = loaded(json!({"languages":{"py":false,"js":true,"rb":true,"jl":false}})).await.settings;
    let env = Environment::from([
        ("SENPI_CODEMODE_PY".into(), "1".into()), ("SENPI_CODEMODE_JS".into(), "0".into()),
        ("SENPI_CODEMODE_RB".into(), "false".into()), ("SENPI_CODEMODE_JL".into(), "true".into()),
    ]);
    assert_eq!(resolve_enabled_languages(&settings, &env), Languages { py: true, js: false, rb: false, jl: true });
}

#[tokio::test]
async fn unknown_settings_rejected() {
    let result = loaded(json!({"unknown":true})).await;
    assert_eq!(result.settings, CodemodeSettings::default());
    assert_eq!(result.warnings.len(), 1);
}

#[tokio::test]
async fn nested_unknown_rejected() {
    assert_eq!(loaded(json!({"languages":{"unknown":false}})).await.warnings.len(), 1);
}

#[tokio::test]
async fn malformed_values_rejected() {
    let result = loaded(json!({"outputSink":{"headBytes":-1},"statusEvents":"yes"})).await;
    assert_eq!(result.settings, CodemodeSettings::default());
    assert_eq!(result.warnings.len(), 1);
}

#[tokio::test]
async fn malformed_json_defaults() {
    let root = tempfile::tempdir().unwrap();
    write_config(root.path(), "{not-json").await;
    let result = load_codemode_settings(root.path(), root.path()).await.unwrap();
    assert_eq!(result.settings, CodemodeSettings::default());
    assert_eq!(result.warnings.len(), 1);
}

#[tokio::test]
async fn invalid_language_and_timeout_rejected() {
    assert_eq!(loaded(json!({"languages":{"py":"yes"},"cellTimeoutSeconds":0})).await.warnings.len(), 1);
}

#[tokio::test]
async fn positive_fractional_capacity_preserved() {
    let result = loaded(json!({"maxDetachedCells":2.5})).await;
    assert!(result.warnings.is_empty());
    assert!((result.settings.max_detached_cells - 2.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn zero_capacity_rejected() { assert_eq!(loaded(json!({"maxDetachedCells":0})).await.warnings.len(), 1); }

#[tokio::test]
async fn negative_capacity_rejected() { assert_eq!(loaded(json!({"maxDetachedCells":-1})).await.warnings.len(), 1); }

#[test]
fn hard_limit_default() { assert!((CodemodeSettings::default().hard_limit_seconds - 1800.0).abs() < f64::EPSILON); }

#[tokio::test]
async fn hard_limit_file() { assert!((loaded(json!({"hardLimitSeconds":90})).await.settings.hard_limit_seconds - 90.0).abs() < f64::EPSILON); }

#[test]
fn hard_limit_env() {
    let env = Environment::from([(HARD_LIMIT_ENVIRONMENT_FLAG.into(), "45".into())]);
    assert!((resolve_hard_limit_seconds(&CodemodeSettings::default(), &env) - 45.0).abs() < f64::EPSILON);
}

#[test]
fn malformed_hard_limit_env_ignored() {
    for value in ["0", "-5", "abc", ""] {
        let env = Environment::from([(HARD_LIMIT_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!((resolve_hard_limit_seconds(&CodemodeSettings::default(), &env) - 1800.0).abs() < f64::EPSILON);
    }
}

#[test]
fn run_budget_default() { assert!((CodemodeSettings::default().run_budget_seconds - 300.0).abs() < f64::EPSILON); }

#[tokio::test]
async fn run_budget_file_and_env() {
    let settings = loaded(json!({"runBudgetSeconds":45})).await.settings;
    assert!((resolve_run_budget_seconds(&settings, &Environment::new()) - 45.0).abs() < f64::EPSILON);
    let env = Environment::from([(RUN_BUDGET_ENVIRONMENT_FLAG.into(), "20".into())]);
    assert!((resolve_run_budget_seconds(&settings, &env) - 20.0).abs() < f64::EPSILON);
}

#[test]
fn foreground_default() { assert!((CodemodeSettings::default().foreground_window_seconds - 60.0).abs() < f64::EPSILON); }

#[tokio::test]
async fn foreground_file() { assert!((loaded(json!({"foregroundWindowSeconds":15})).await.settings.foreground_window_seconds - 15.0).abs() < f64::EPSILON); }

#[test]
fn foreground_env() {
    let env = Environment::from([(FOREGROUND_WINDOW_ENVIRONMENT_FLAG.into(), "7".into())]);
    assert!((resolve_foreground_window_seconds(&CodemodeSettings::default(), &env) - 7.0).abs() < f64::EPSILON);
}

#[test]
fn foreground_invalid_env_ignored() {
    for value in ["0", "-5", "abc", ""] {
        let env = Environment::from([(FOREGROUND_WINDOW_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!((resolve_foreground_window_seconds(&CodemodeSettings::default(), &env) - 60.0).abs() < f64::EPSILON);
    }
}

#[test]
fn capacity_env_parse_int_contract() {
    for (value, expected) in [("+2.5", 2.0), ("4suffix", 4.0), ("0", 15.0), ("bad", 15.0)] {
        let env = Environment::from([(MAX_DETACHED_CELLS_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!((resolve_max_detached_cells(&CodemodeSettings::default(), &env) - expected).abs() < f64::EPSILON);
    }
}

#[test]
fn language_env_case_whitespace_and_unknown() {
    let env = Environment::from([("SENPI_CODEMODE_PY".into(), " FALSE ".into()), ("SENPI_CODEMODE_JS".into(), "unknown".into())]);
    let languages = resolve_enabled_languages(&CodemodeSettings::default(), &env);
    assert!(!languages.py);
    assert!(languages.js);
}
