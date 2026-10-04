use maho_cli::cli::{args::{Args, Mode}, startup::*};
use maho_core::project_trust::AppMode;
#[test]
fn mode_selection_respects_explicit_protocols_and_terminal_state() {
    assert_eq!(resolve_app_mode(&Args::default(), true, true), AppMode::Interactive);
    assert_eq!(resolve_app_mode(&Args::default(), false, true), AppMode::Print);
    assert_eq!(resolve_app_mode(&Args { mode: Some(Mode::Rpc), print: true, ..Default::default() }, false, false), AppMode::Rpc);
    assert_eq!(resolve_app_mode(&Args { mode: Some(Mode::Json), ..Default::default() }, true, true), AppMode::Json);
    assert_eq!(resolve_app_mode(&Args { messages: vec!["app-server".to_owned()], ..Default::default() }, true, true), AppMode::AppServer);
}
#[test]
fn auto_title_preserves_context_and_session_override_precedence() {
    let parsed = Args::default();
    assert!(resolve_auto_title_sessions(AppMode::Interactive, &parsed, false, &[], None));
    assert!(!resolve_auto_title_sessions(AppMode::Interactive, &parsed, true, &[], Some(true)));
    assert!(!resolve_auto_title_sessions(AppMode::Interactive, &parsed, false, &[], Some(false)));
    assert!(resolve_auto_title_sessions(AppMode::Rpc, &parsed, false, &["auto_title_sessions".to_owned()], None));
    assert!(!resolve_auto_title_sessions(AppMode::Print, &parsed, false, &[], None));
}
#[test]
fn session_creation_flags_reject_conflicts_and_unsafe_ids() {
    assert_eq!(validate_fork_flags(&Args { fork: Some("source".to_owned()), continue_session: true, no_session: true, ..Default::default() }).unwrap_err(), "--fork cannot be combined with --continue, --no-session");
    for id in ["", "../escape", "-bad", "bad."] { assert!(validate_session_id_flags(&Args { session_id: Some(id.to_owned()), ..Default::default() }).is_err()); }
    assert!(validate_session_id_flags(&Args { session_id: Some("a.b_c-2".to_owned()), no_session: true, ..Default::default() }).is_ok());
}
#[tokio::test]
async fn prepared_message_uses_real_file_and_keeps_private_input_out_of_title() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("input.txt"), "private").unwrap();
    let mut parsed = Args { file_args: vec!["input.txt".to_owned()], messages: vec!["question".to_owned(), "next".to_owned()], ..Default::default() };
    let result = prepare_initial_message(&mut parsed, dir.path(), false, None).await.unwrap();
    assert!(result.initial_message.unwrap().contains("private")); assert!(result.initial_title_prompt.is_none()); assert_eq!(parsed.messages, ["next"]);
}
#[test]
fn scoped_startup_model_and_tool_suppression_preserve_explicit_thinking_off() {
    use maho_ai::types::ModelThinkingLevel;
    let dir = tempfile::tempdir().unwrap(); let cwd = dir.path().to_string_lossy();
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { models_path: Some(dir.path().join("models.json")), auth_path: Some(dir.path().join("auth.json")), ..Default::default() });
    let model = runtime.get_models(None).remove(0);
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &dir.path().join("agent").to_string_lossy(), &cwd, false);
    let scoped = [maho_core::model_resolver::ScopedModel { model: model.clone(), thinking_level: Some(ModelThinkingLevel::High), thinking_selection: None, service_tier: Some("priority".to_owned()) }];
    let parsed = Args { thinking: Some("off".to_owned()), no_builtin_tools: true, tools: Some(vec!["read".to_owned()]), ..Default::default() };
    let built = build_session_options(&parsed, &scoped, false, &runtime, &settings);
    assert_eq!(built.options.model.unwrap().id, model.id); assert_eq!(built.options.thinking_level, Some(ModelThinkingLevel::Off));
    assert!(matches!(built.options.no_tools, Some(maho_core::sdk::NoToolsMode::Builtin))); assert_eq!(built.options.tools.unwrap(), ["read"]);
    assert!(build_session_options(&parsed, &scoped, true, &runtime, &settings).options.model.is_none());
    let entries = session_model_entries(scoped.to_vec()).unwrap();
    assert_eq!(entries[0].model.id, model.id);
    assert_eq!(entries[0].thinking_level, Some(maho_ai::types::ThinkingLevel::High));
    assert_eq!(entries[0].service_tier, Some(maho_ext_api::ServiceTier::Priority));
    let selection = maho_ai::types::ThinkingSelection {
        level: ModelThinkingLevel::Off,
        source: maho_ai::types::ThinkingSelectionSource::Explicit,
        legacy_variant_id: None,
    };
    let off = session_model_entries(vec![maho_core::model_resolver::ScopedModel {
        model, thinking_level: Some(ModelThinkingLevel::Off),
        thinking_selection: Some(selection.clone()), service_tier: None,
    }]).unwrap();
    assert_eq!(off[0].thinking_level, None);
    assert_eq!(off[0].thinking_selection, Some(selection));
}

#[test]
fn startup_theme_respects_terminal_background_and_auto_pairs() {
    use maho_cli::cli::startup_ui::resolve_startup_theme;
    assert_eq!(resolve_startup_theme(None, Some("0;15")).expect("light terminal").name, "light");
    assert_eq!(resolve_startup_theme(None, Some("15;0")).expect("dark terminal").name, "dark");
    assert_eq!(resolve_startup_theme(Some("light/dark"), Some("0;15")).expect("auto light").name, "light");
    assert_eq!(resolve_startup_theme(Some("light/dark"), Some("15;0")).expect("auto dark").name, "dark");
    assert_eq!(resolve_startup_theme(Some("light"), Some("15;0")).expect("explicit light").name, "light");
    assert_eq!(resolve_startup_theme(Some("missing-theme"), None).expect("pinned dark fallback").name, "dark");
}

fn omarchy_style_theme(name: &str) -> String {
    let mut colors = serde_json::Map::new();
    for color in maho_interactive::theme::ThemeColor::ALL {
        colors.insert(color.key().to_owned(), serde_json::json!("foreground"));
    }
    for background in maho_interactive::theme::ThemeBg::ALL {
        colors.insert(background.key().to_owned(), serde_json::json!("background"));
    }
    let mut document = serde_json::Map::new();
    document.insert("name".into(), serde_json::Value::String(name.to_owned()));
    document.insert("vars".into(), serde_json::json!({ "foreground": "#a9b1d6", "background": "#1a1b26" }));
    document.insert("colors".into(), serde_json::Value::Object(colors));
    serde_json::Value::Object(document).to_string()
}

#[test]
fn startup_theme_registry_loads_custom_resources_and_reports_fallbacks() {
    use maho_cli::cli::startup_ui::{load_theme_resources, resolve_startup_theme_with_registered};
    use maho_interactive::theme::ColorMode;
    let directory = tempfile::tempdir().expect("custom themes dir");
    std::fs::write(directory.path().join("omarchy-system.json"), omarchy_style_theme("omarchy-system")).expect("valid custom theme");
    let custom = resolve_startup_theme_with_registered(Some("omarchy-system"), None, directory.path(), Vec::new()).expect("resolution");
    assert_eq!(custom.theme.name, "omarchy-system");
    assert!(custom.diagnostics.is_empty());

    let missing = resolve_startup_theme_with_registered(Some("absent-theme"), None, directory.path(), Vec::new()).expect("resolution");
    assert_eq!(missing.theme.name, "dark");
    assert!(!missing.diagnostics.is_empty(), "missing theme diagnostic must not be hidden");

    std::fs::write(directory.path().join("broken.json"), "{ not json").expect("invalid custom theme");
    let broken = resolve_startup_theme_with_registered(Some("broken"), None, directory.path(), Vec::new()).expect("resolution");
    assert_eq!(broken.theme.name, "dark");
    assert!(!broken.diagnostics.is_empty(), "invalid theme diagnostic must not be hidden");

    let resource_directory = tempfile::tempdir().expect("resource dir");
    let resource_path = resource_directory.path().join("packaged.json");
    std::fs::write(&resource_path, omarchy_style_theme("packaged")).expect("resource theme");
    let (registered, diagnostics) = load_theme_resources([resource_path], ColorMode::Truecolor);
    assert!(diagnostics.is_empty());
    let packaged = resolve_startup_theme_with_registered(Some("packaged"), None, directory.path(), registered).expect("resolution");
    assert_eq!(packaged.theme.name, "packaged");
    assert!(packaged.diagnostics.is_empty());
}
