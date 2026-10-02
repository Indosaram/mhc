use super::{args::{Args, Mode}, initial_message::{build_initial_message, InitialMessageResult}};
use maho_core::project_trust::AppMode;
pub fn resolve_app_mode(parsed: &Args, stdin_is_tty: bool, stdout_is_tty: bool) -> AppMode {
    if parsed.mode.is_none() && parsed.messages.first().is_some_and(|message| message == "app-server") { return AppMode::AppServer; }
    match parsed.mode { Some(Mode::Rpc) => AppMode::Rpc, Some(Mode::Json) => AppMode::Json, Some(Mode::Text) | None => {
        if parsed.print || !stdin_is_tty || !stdout_is_tty { AppMode::Print } else { AppMode::Interactive }
    } }
}
pub fn resolve_auto_title_sessions(mode: AppMode, parsed: &Args, has_context_messages: bool, capabilities: &[String], session_auto_title: Option<bool>) -> bool {
    if has_context_messages { return false; }
    session_auto_title.unwrap_or_else(|| mode == AppMode::Interactive || parsed.auto_title_sessions || (mode == AppMode::Rpc && capabilities.iter().any(|capability| capability == "auto_title_sessions")))
}
pub fn validate_fork_flags(parsed: &Args) -> Result<(), String> {
    if parsed.fork.as_ref().is_none_or(|fork| fork.is_empty()) { return Ok(()); }
    let conflicts: Vec<_> = [(parsed.session.as_ref().is_some_and(|session| !session.is_empty()), "--session"), (parsed.continue_session, "--continue"), (parsed.resume, "--resume"), (parsed.no_session, "--no-session")].into_iter().filter_map(|(set, flag)| set.then_some(flag)).collect();
    if conflicts.is_empty() { Ok(()) } else { Err(format!("--fork cannot be combined with {}", conflicts.join(", "))) }
}
pub fn validate_session_id_flags(parsed: &Args) -> Result<(), String> {
    let Some(id) = &parsed.session_id else { return Ok(()); };
    let conflicts: Vec<_> = [(parsed.session.as_ref().is_some_and(|session| !session.is_empty()), "--session"), (parsed.continue_session, "--continue"), (parsed.resume, "--resume")].into_iter().filter_map(|(set, flag)| set.then_some(flag)).collect();
    if !conflicts.is_empty() { return Err(format!("--session-id cannot be combined with {}", conflicts.join(", "))); }
    let bytes = id.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_alphanumeric) && bytes.last().is_some_and(u8::is_ascii_alphanumeric) && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) { Ok(()) }
    else { Err("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_owned()) }
}
pub async fn prepare_initial_message(parsed: &mut Args, cwd: &std::path::Path, auto_resize_images: bool, stdin_content: Option<&str>) -> Result<InitialMessageResult, String> {
    if parsed.file_args.is_empty() { return Ok(build_initial_message(parsed, None, None, stdin_content)); }
    let files = super::file_processor::process_file_arguments(&parsed.file_args, cwd, Some(auto_resize_images)).await?;
    Ok(build_initial_message(parsed, Some(&files.text), Some(files.images), stdin_content))
}
pub struct SessionOptions {
    pub model: Option<maho_ai::types::Model>,
    pub initial_model_provenance: Option<&'static str>,
    pub thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
    pub scoped_models: Vec<maho_core::model_resolver::ScopedModel>,
    pub no_tools: Option<maho_core::sdk::NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
}
pub struct BuiltSessionOptions { pub options: SessionOptions, pub cli_thinking_from_model: bool, pub diagnostics: Vec<super::args::Diagnostic> }
pub fn build_session_options(parsed: &Args, scoped: &[maho_core::model_resolver::ScopedModel], has_existing_session: bool, runtime: &maho_core::model_runtime::ModelRuntime, settings: &maho_core::settings_manager::SettingsManager) -> BuiltSessionOptions {
    use maho_ai::types::{ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};
    let mut options = SessionOptions { model: None, initial_model_provenance: None, thinking_level: None, thinking_selection: None, scoped_models: scoped.to_vec(), no_tools: None, tools: parsed.tools.clone(), exclude_tools: parsed.exclude_tools.clone() };
    let mut diagnostics = Vec::new(); let mut cli_thinking_from_model = false;
    let thinking = parsed.thinking.as_deref().and_then(ModelThinkingLevel::parse);
    if parsed.model.as_ref().is_some_and(|model| !model.is_empty()) {
        let resolved = maho_core::model_resolver::resolve_cli_model(parsed.provider.as_deref(), parsed.model.as_deref(), thinking, runtime);
        if let Some(message) = resolved.parsed.warning { diagnostics.push(super::args::Diagnostic { error: false, message }); }
        if let Some(message) = resolved.error { diagnostics.push(super::args::Diagnostic { error: true, message }); }
        if let Some(model) = resolved.parsed.model {
            options.model = Some(model); options.initial_model_provenance = Some("cli");
            if thinking.is_none() && resolved.parsed.thinking_level.is_some() { options.thinking_level = resolved.parsed.thinking_level; options.thinking_selection = resolved.parsed.thinking_selection; cli_thinking_from_model = true; }
        }
    }
    if options.model.is_none() && !has_existing_session && let Some(first) = scoped.first() {
        let saved = settings.get_string("defaultProvider").zip(settings.get_string("defaultModel")).and_then(|(provider, id)| runtime.get_model(&provider, &id));
        let selected = saved.and_then(|saved| scoped.iter().find(|candidate| candidate.model.provider == saved.provider && candidate.model.id == saved.id)).unwrap_or(first);
        options.model = Some(selected.model.clone()); options.initial_model_provenance = Some("scoped");
        if thinking.is_none() { options.thinking_level = selected.thinking_level; options.thinking_selection = selected.thinking_selection.clone(); }
    }
    if let Some(level) = thinking { options.thinking_level = Some(level); options.thinking_selection = Some(ThinkingSelection { level, source: ThinkingSelectionSource::Explicit, legacy_variant_id: None }); }
    options.no_tools = if parsed.no_tools { Some(maho_core::sdk::NoToolsMode::All) } else if parsed.no_builtin_tools { Some(maho_core::sdk::NoToolsMode::Builtin) } else { None };
    BuiltSessionOptions { options, cli_thinking_from_model, diagnostics }
}
