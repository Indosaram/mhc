use maho_ai::types::ModelThinkingLevel;
use maho_core::settings_manager::{Settings, SettingsManager, SettingsScope};
use maho_ext_api::Model;
use maho_cli::cli::reasoning_host::NativeReasoningHost;
use serde_json::{json, Map, Value};

fn model() -> Model {
    serde_json::from_value(json!({
        "id": "model-a", "name": "Model A", "api": "openai-completions", "provider": "provider-a",
        "baseUrl": "", "reasoning": true, "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "contextWindow": 1000, "maxTokens": 100
    })).expect("model fixture")
}

#[test]
fn preferences_read_durable_last_on_remembered_and_default_separately() {
    let root = tempfile::tempdir().expect("temporary settings root");
    let mut manager = SettingsManager::create(
        root.path().to_str().expect("cwd"),
        root.path().join("agent").to_str().expect("agent dir"),
        root.path().to_str().expect("home"),
        false,
    );
    manager.apply_overrides(&Map::from_iter([
        ("modelThinkingLevels".to_owned(), json!({"provider-a/model-a": "high"})),
        ("modelLastOnThinkingLevels".to_owned(), json!({"provider-a/model-a": "xhigh"})),
        ("defaultThinkingLevel".to_owned(), json!("low")),
    ]));
    let model = model();
    assert_eq!(NativeReasoningHost::remembered_level(&manager, &model), Some(ModelThinkingLevel::High));
    assert_eq!(NativeReasoningHost::last_on_level(&manager, &model), Some(ModelThinkingLevel::Xhigh));
    assert_eq!(NativeReasoningHost::default_level(&manager), Some(ModelThinkingLevel::Low));
}

#[test]
fn model_level_persistence_preserves_other_model_and_global_values() {
    let root = tempfile::tempdir().expect("temporary settings root");
    let cwd = root.path().to_string_lossy();
    let agent_dir = root.path().join("agent").to_string_lossy().into_owned();
    let mut manager = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
    let mut models = Map::new();
    models.insert("provider-a/model-a".into(), json!("low"));
    models.insert("provider-b/model-b".into(), json!("xhigh"));
    manager.set(SettingsScope::Global, &Settings::from_iter([
        ("modelThinkingLevels".to_owned(), Value::Object(models)),
        ("defaultThinkingLevel".to_owned(), json!("low")),
    ])).expect("persist preferences");
    let reloaded = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
    assert_eq!(NativeReasoningHost::remembered_level(&reloaded, &model()), Some(ModelThinkingLevel::Low));
    assert_eq!(NativeReasoningHost::default_level(&reloaded), Some(ModelThinkingLevel::Low));
    assert_eq!(reloaded.get_value("modelThinkingLevels").and_then(|value| value.get("provider-b/model-b"))
        .and_then(Value::as_str), Some("xhigh"));
}
