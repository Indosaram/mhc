use maho_ext_api::{BuildSystemPromptOptions, EventKind, Extension, ExtensionApi, ExtensionSessionProfile, LoadedExtension, SourceInfo, EventBus, ExtensionRuntime};
use maho_ext_prompt_preset::index::{has_user_system_prompt, with_user_appends, resolve_home_with};

#[test]
fn registers_prompt_and_model_hooks() {
    let mut api = ExtensionApi::new(LoadedExtension::new("preset", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    assert_eq!(api.registered.handlers[&EventKind::BeforeAgentStart].len(), 1);
    assert_eq!(api.registered.handlers[&EventKind::ModelSelect].len(), 1);
}
#[test]
fn custom_prompt_truthiness_and_append_preservation() {
    let mut options = BuildSystemPromptOptions::default();
    assert!(!has_user_system_prompt(&options));
    options.custom_prompt = Some(String::new());
    assert!(!has_user_system_prompt(&options));
    options.custom_prompt = Some(" ".into());
    assert!(has_user_system_prompt(&options));
    options.append_system_prompt = Some("\nfixture ".into());
    assert_eq!(with_user_appends("base".into(), &options), "base\n\n\nfixture ");
    options.append_system_prompt = Some(String::new());
    assert_eq!(with_user_appends("base".into(), &options), "base");
}
#[test]
fn settings_respects_userprofile_when_home_unset() {
    let fixture_home = std::env::temp_dir().join(format!("maho_preset_test_{}", std::process::id()));
    let senpi_dir = fixture_home.join(".senpi");
    let _ = std::fs::create_dir_all(&senpi_dir);
    let _ = std::fs::write(senpi_dir.join("settings.json"), r#"{"promptPreset": "gpt-5.5"}"#);
    let home = resolve_home_with(|key| match key {
        "HOME" => Err(std::env::VarError::NotPresent),
        "USERPROFILE" => Ok(fixture_home.to_string_lossy().into_owned()),
        _ => Err(std::env::VarError::NotPresent),
    });
    assert_eq!(home, fixture_home.to_string_lossy());
    let settings = maho_core::settings_manager::SettingsManager::create(
        "/tmp",
        "/tmp/agent",
        &home,
        false,
    );
    let loaded = maho_ext_prompt_preset::settings::load_prompt_preset_settings(
        None,
        settings.get_global().get("promptPreset").and_then(|v| v.as_str()),
    );
    assert_eq!(loaded, maho_ext_prompt_preset::settings::PromptPresetName::Gpt55);
    let _ = std::fs::remove_dir_all(&fixture_home);
}
