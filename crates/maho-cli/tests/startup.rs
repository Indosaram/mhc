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
    let scoped = [maho_core::model_resolver::ScopedModel { model: model.clone(), thinking_level: Some(ModelThinkingLevel::High), thinking_selection: None, service_tier: None }];
    let parsed = Args { thinking: Some("off".to_owned()), no_builtin_tools: true, tools: Some(vec!["read".to_owned()]), ..Default::default() };
    let built = build_session_options(&parsed, &scoped, false, &runtime, &settings);
    assert_eq!(built.options.model.unwrap().id, model.id); assert_eq!(built.options.thinking_level, Some(ModelThinkingLevel::Off));
    assert!(matches!(built.options.no_tools, Some(maho_core::sdk::NoToolsMode::Builtin))); assert_eq!(built.options.tools.unwrap(), ["read"]);
    assert!(build_session_options(&parsed, &scoped, true, &runtime, &settings).options.model.is_none());
}
