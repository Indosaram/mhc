use maho_codemode::config::settings::*;
use serde_json::json;
use std::path::Path;

async fn write_config(root: &Path, value: &str) {
    tokio::fs::create_dir_all(root.join(".omo"))
        .await
        .expect("create test config directory");
    tokio::fs::write(root.join(".omo/codemode.json"), value)
        .await
        .expect("write test config");
}

async fn loaded(value: serde_json::Value) -> LoadedCodemodeSettings {
    let root = tempfile::tempdir().expect("create temporary config root");
    write_config(root.path(), &value.to_string()).await;
    load_codemode_settings(root.path(), root.path())
        .await
        .expect("read test config")
}

#[tokio::test]
async fn defaults_without_file() {
    let root = tempfile::tempdir().unwrap();
    let settings = load_codemode_settings(root.path(), root.path())
        .await
        .unwrap();
    assert_eq!(settings.settings, CodemodeSettings::default());
    assert!(settings.source.is_none());
    assert!(settings.warnings.is_empty());
}

#[tokio::test]
async fn project_precedes_global() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let home = root.path().join("home");
    write_config(
        &project,
        r#"{"languages":{"js":true},"parallelPoolWidth":9,"maxDetachedCells":2}"#,
    )
    .await;
    tokio::fs::create_dir_all(home.join(".omo/agent"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".omo/agent/codemode.json"),
        r#"{"languages":{"js":false}}"#,
    )
    .await
    .unwrap();
    let result = load_codemode_settings(&project, &home).await.unwrap();
    assert_eq!(result.source, Some(project.join(".omo/codemode.json")));
    assert_eq!(result.settings.languages, Languages { js: true });
    assert!((result.settings.parallel_pool_width - 9.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn global_fallback() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join(".omo/agent"))
        .await
        .unwrap();
    let file = root.path().join(".omo/agent/codemode.json");
    tokio::fs::write(
        &file,
        r#"{"languages":{"js":false},"cellTimeoutSeconds":12}"#,
    )
    .await
    .unwrap();
    let result = load_codemode_settings(&root.path().join("missing"), root.path())
        .await
        .unwrap();
    assert_eq!(result.source, Some(file));
    assert!(!result.settings.languages.js);
    assert!((result.settings.cell_timeout_seconds - 12.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn legacy_language_keys_are_ignored() {
    let result = loaded(json!({"languages":{"py":false,"js":true,"rb":true,"jl":false}})).await;
    assert!(result.warnings.is_empty());
    assert_eq!(result.settings.languages, Languages { js: true });
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
    assert!(
        !loaded(json!({"statusEvents":false}))
            .await
            .settings
            .status_events
    );
}

#[tokio::test]
async fn language_env_beats_file() {
    let settings = loaded(json!({"languages":{"js":true}})).await.settings;
    let env = Environment::from([("SENPI_CODEMODE_JS".into(), "0".into())]);
    assert_eq!(
        resolve_enabled_languages(&settings, &env),
        Languages { js: false }
    );
}

#[tokio::test]
async fn unknown_top_level_key_is_dropped_with_one_warning() {
    // config.test.ts: a key from a newer version keeps the other settings and warns once.
    let result = loaded(json!({"futureKey":1,"runBudgetSeconds":120})).await;
    assert_eq!(result.settings.run_budget_seconds, 120.0);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].starts_with("Unknown codemode setting \"futureKey\" in "));
}

#[tokio::test]
async fn unknown_nested_key_still_falls_back_to_defaults() {
    let result =
        loaded(json!({"runBudgetSeconds":120,"taskTools":{"task":"task","futureNested":true}}))
            .await;
    assert_eq!(result.settings, CodemodeSettings::default());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("Falling back to codemode defaults"))
    );
}

#[tokio::test]
async fn nested_unknown_rejected() {
    assert_eq!(
        loaded(json!({"languages":{"unknown":false}}))
            .await
            .warnings
            .len(),
        1
    );
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
    let result = load_codemode_settings(root.path(), root.path())
        .await
        .unwrap();
    assert_eq!(result.settings, CodemodeSettings::default());
    assert_eq!(result.warnings.len(), 1);
}

#[tokio::test]
async fn invalid_language_and_timeout_rejected() {
    assert_eq!(
        loaded(json!({"languages":{"js":"yes"},"cellTimeoutSeconds":0}))
            .await
            .warnings
            .len(),
        1
    );
}

#[tokio::test]
async fn positive_fractional_capacity_preserved() {
    let result = loaded(json!({"maxDetachedCells":2.5})).await;
    assert!(result.warnings.is_empty());
    assert!((result.settings.max_detached_cells - 2.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn zero_capacity_rejected() {
    assert_eq!(
        loaded(json!({"maxDetachedCells":0})).await.warnings.len(),
        1
    );
}

#[tokio::test]
async fn negative_capacity_rejected() {
    assert_eq!(
        loaded(json!({"maxDetachedCells":-1})).await.warnings.len(),
        1
    );
}

#[test]
fn hard_limit_default() {
    assert!((CodemodeSettings::default().hard_limit_seconds - 1800.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn hard_limit_file() {
    assert!(
        (loaded(json!({"hardLimitSeconds":90}))
            .await
            .settings
            .hard_limit_seconds
            - 90.0)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn hard_limit_env() {
    let env = Environment::from([(HARD_LIMIT_ENVIRONMENT_FLAG.into(), "45".into())]);
    assert!(
        (resolve_hard_limit_seconds(&CodemodeSettings::default(), &env) - 45.0).abs()
            < f64::EPSILON
    );
}

#[test]
fn malformed_hard_limit_env_ignored() {
    for value in ["0", "-5", "abc", ""] {
        let env = Environment::from([(HARD_LIMIT_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!(
            (resolve_hard_limit_seconds(&CodemodeSettings::default(), &env) - 1800.0).abs()
                < f64::EPSILON
        );
    }
}

#[test]
fn run_budget_default() {
    assert!((CodemodeSettings::default().run_budget_seconds - 300.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn run_budget_file_and_env() {
    let settings = loaded(json!({"runBudgetSeconds":45})).await.settings;
    assert!(
        (resolve_run_budget_seconds(&settings, &Environment::new()) - 45.0).abs() < f64::EPSILON
    );
    let env = Environment::from([(RUN_BUDGET_ENVIRONMENT_FLAG.into(), "20".into())]);
    assert!((resolve_run_budget_seconds(&settings, &env) - 20.0).abs() < f64::EPSILON);
}

#[test]
fn foreground_default() {
    assert!((CodemodeSettings::default().foreground_window_seconds - 60.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn foreground_file() {
    assert!(
        (loaded(json!({"foregroundWindowSeconds":15}))
            .await
            .settings
            .foreground_window_seconds
            - 15.0)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn foreground_env() {
    let env = Environment::from([(FOREGROUND_WINDOW_ENVIRONMENT_FLAG.into(), "7".into())]);
    assert!(
        (resolve_foreground_window_seconds(&CodemodeSettings::default(), &env) - 7.0).abs()
            < f64::EPSILON
    );
}

#[test]
fn foreground_invalid_env_ignored() {
    for value in ["0", "-5", "abc", ""] {
        let env = Environment::from([(FOREGROUND_WINDOW_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!(
            (resolve_foreground_window_seconds(&CodemodeSettings::default(), &env) - 60.0).abs()
                < f64::EPSILON
        );
    }
}

#[test]
fn capacity_env_parse_int_contract() {
    for (value, expected) in [("+2.5", 2.0), ("4suffix", 4.0), ("0", 15.0), ("bad", 15.0)] {
        let env = Environment::from([(MAX_DETACHED_CELLS_ENVIRONMENT_FLAG.into(), value.into())]);
        assert!(
            (resolve_max_detached_cells(&CodemodeSettings::default(), &env) - expected).abs()
                < f64::EPSILON
        );
    }
}

#[test]
fn language_env_case_whitespace_and_unknown() {
    let env = Environment::from([("SENPI_CODEMODE_JS".into(), "unknown".into())]);
    let languages = resolve_enabled_languages(&CodemodeSettings::default(), &env);
    assert!(languages.js);
    assert!(languages.js);
}

/// senpi config.test.ts `it.each(...)("enforces the $name through the tool's detached manager")`: the
/// resolved capacity (file, then environment override) is what the real detached-cell manager admits.
#[tokio::test]
async fn resolved_capacity_is_enforced_by_the_detached_manager() {
    use maho_codemode::tool::{
        detached_cell_manager::{DetachedCellManagerOptions, EvalDetachedCellManager},
        types::{EvalLanguage, EvalToolInput},
    };
    use maho_ext_api::AgentToolResult;
    use std::sync::Arc;
    let cases: [(&str, serde_json::Value, Option<&str>, usize); 5] = [
        ("default cap", json!({}), None, 15),
        ("file cap", json!({"maxDetachedCells":4}), None, 4),
        (
            "environment cap",
            json!({"maxDetachedCells":4}),
            Some("2"),
            2,
        ),
        (
            "invalid environment cap",
            json!({"maxDetachedCells":4}),
            Some("0"),
            4,
        ),
        (
            "malformed environment cap",
            json!({"maxDetachedCells":4}),
            Some("bad"),
            4,
        ),
    ];
    for (name, file, env, capacity) in cases {
        let loaded = loaded(file).await;
        assert!(loaded.warnings.is_empty(), "{name}");
        let env = env.map_or_else(Environment::new, |value| {
            Environment::from([(MAX_DETACHED_CELLS_ENVIRONMENT_FLAG.into(), value.into())])
        });
        let resolved = resolve_max_detached_cells(&loaded.settings, &env);
        let mut manager = EvalDetachedCellManager::new(DetachedCellManagerOptions {
            max_detached_cells: resolved.ceil() as usize,
            ..Default::default()
        });
        let mut admitted = Vec::new();
        let mut cells = Vec::new();
        for index in 0..=capacity {
            let input = EvalToolInput {
                language: EvalLanguage::Js,
                code: format!("cap-{index}"),
                summary: format!("cap-{index}"),
                action: None,
                timeout: None,
                on_timeout: None,
                reset: None,
                isolate: None,
            };
            let cell = manager
                .create(format!("cap-{index}"), input)
                .expect("create cell");
            manager.bind_kernel(
                &cell,
                Arc::new(|| AgentToolResult::text("")),
                Arc::new(|| (None, vec![])),
            );
            admitted.push(manager.detach(&cell));
            cells.push(cell);
        }
        let mut expected = vec![true; capacity];
        expected.push(false);
        assert_eq!(admitted, expected, "{name}");
        for cell in &cells {
            manager.cancel_without_interrupt(cell);
        }
        manager.flush_notifications().await.expect("flush");
    }
}

/// senpi 903b8336 config.test.ts: settled-result and image-spill budgets resolve from defaults, the
/// file and the environment (only a whole non-negative number of MiB overrides).
#[tokio::test]
async fn retained_budgets_resolve_from_defaults_file_and_environment() {
    use maho_codemode::config::memory_settings::*;
    let loaded = loaded(json!({"memory":{"retainedResultsMb":8,"retainedImagesMb":64}})).await;
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    const MIB: u64 = 1024 * 1024;
    let defaults = CodemodeSettings::default();
    let cases: [(fn(&CodemodeSettings, &Environment) -> u64, &str, u64, u64); 2] = [
        (resolve_retained_results_bytes, RETAINED_RESULTS_ENVIRONMENT_FLAG, 32, 8),
        (resolve_retained_images_bytes, RETAINED_IMAGES_ENVIRONMENT_FLAG, 256, 64),
    ];
    for (resolve, flag, fallback, from_file) in cases {
        let env = |value: &str| Environment::from([(flag.to_owned(), value.to_owned())]);
        assert_eq!(resolve(&defaults, &Environment::new()), fallback * MIB);
        assert_eq!(resolve(&loaded.settings, &Environment::new()), from_file * MIB);
        assert_eq!(resolve(&loaded.settings, &env("4")), 4 * MIB);
        assert_eq!(resolve(&loaded.settings, &env("0")), 0);
        for value in ["-1", "abc", "", "1.5"] {
            assert_eq!(resolve(&loaded.settings, &env(value)), from_file * MIB, "{value}");
        }
    }
}

/// senpi 903b8336 memory-settings.test.ts: file validation of the memory object.
#[tokio::test]
async fn memory_file_values_validate_and_out_of_order_thresholds_warn() {
    let disordered = loaded(json!({"memory":{"noticeMb":4096,"ceilingMb":2048}})).await;
    assert_eq!(disordered.warnings.len(), 1);
    assert_eq!(disordered.settings.memory(), maho_codemode::config::memory_settings::default_memory_settings());
    let negative = loaded(json!({"memory":{"ceilingMb":-1}})).await;
    assert_eq!(negative.warnings.len(), 1);
    assert_eq!(negative.settings, CodemodeSettings::default());
    let park = loaded(json!({"memory":{"idleParkMinutes":5}})).await;
    assert!(park.warnings.is_empty());
    assert_eq!(park.settings.memory().idle_park_minutes, Some(5.0));
    let unset = loaded(json!({"memory":{"noticeMb":512}})).await;
    assert!(unset.warnings.is_empty());
    assert_eq!(unset.settings.memory().idle_park_minutes, None);
    let too_long = loaded(json!({"memory":{"idleParkMinutes":35_792}})).await;
    assert_eq!(too_long.warnings.len(), 1);
    assert_eq!(too_long.settings.memory().idle_park_minutes, None);
}
