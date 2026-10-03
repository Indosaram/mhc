//! Session substitution seam for the interactive host port (`interactive-host-runtime.ts`'s
//! `InteractiveSession` union). `AgentSession`'s `with_settings_manager`/`with_session_manager`
//! are generic closures and cannot live behind a `dyn` trait, so this seam carries only the
//! transport-facing replacement operations; settings and transcript reads stay on the local session.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use maho_core::agent_session::AgentSession;
use maho_core::session_manager::NewSessionOptions;

use crate::interactive_host_runtime::RemoteInteractiveRuntime;

pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `LocalHandoff` means the remote runtime declined and the caller must run the operation against
/// the local session, which a plain `bool` cannot express alongside `Cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementOutcome {
    Replaced,
    Cancelled,
    LocalHandoff,
}

/// The result of a `fork`/`clone`: the replacement outcome plus the selected text a `before` fork
/// returns to prefill the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub outcome: ReplacementOutcome,
    pub editor_text: Option<String>,
}

pub trait InteractiveSession {
    fn session_id(&self) -> Option<String>;
    fn session_file(&self) -> Option<String>;
    fn cwd(&self) -> String;
    fn is_reconnecting(&self) -> bool;
    fn is_fallback(&self) -> bool;
    /// The remote runtime's mirrored `RpcSessionState`, or `None` for a local session. This is the
    /// typed snapshot the mode reads for remote-authoritative values (name, streaming, model).
    fn remote_state(&self) -> Option<crate::interactive_host_runtime::RemoteSessionState>;
    /// Subscribe to decoded remote session events; `None` for a local session. The returned guard
    /// removes the listener when dropped, so a replacement or dispose tears the bridge down.
    fn subscribe_session_events(&self, listener: crate::interactive_host_runtime::SessionEventListener) -> Option<crate::interactive_host_runtime::SessionSubscription>;
    /// Fetch the authoritative remote history and deliver it, generation-tagged, to `sender`. A
    /// no-op for a local session (the mode reads its messages synchronously).
    fn request_remote_history(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>);
    /// Fetch the host's available-model catalog, generation-tagged, to `sender`. A no-op locally.
    fn request_remote_models(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
    /// The turn/session mutations `InteractiveMode` performs. When a shared host is mounted these
    /// route to the host (senpi's proxy arms); otherwise they hit the local session. `messages`,
    /// `modelRegistry`, `resolveToolCallName` and `getLastAssistantText` stay local (senpi does not
    /// override them either).
    fn prompt(&self, _message: String, _options: maho_core::agent_session::PromptOptions) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("prompt is not implemented for this session host".to_owned()) }) }
    fn abort(&self) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("abort is not implemented for this session host".to_owned()) }) }
    fn steer(&self, _text: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("steer is not implemented for this session host".to_owned()) }) }
    fn follow_up(&self, _text: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("follow_up is not implemented for this session host".to_owned()) }) }
    fn compact(&self, _instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("compact is not implemented for this session host".to_owned()) }) }
    fn navigate_tree(&self, _entry_id: String, _options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> { Box::pin(async { Err("navigate_tree is not implemented for this session host".to_owned()) }) }
    fn edit_assistant_message(&self, _entry_id: String, _text: String, _options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> { Box::pin(async { Err("edit_assistant_message is not implemented for this session host".to_owned()) }) }
    fn reload(&self) -> SessionFuture<'_, Result<bool, String>> { Box::pin(async { Err("reload is not implemented for this session host".to_owned()) }) }
    /// senpi's `clearQueue` proxy arm is synchronous: it returns the mirrored queue and fires the
    /// RPC. A no-op locally (the caller reads the local session's queue directly).
    fn fire_clear_queue(&self, _abort_will_follow: bool) {}
    fn execute_bash(&self, _command: String, _exclude_from_context: bool) -> SessionFuture<'_, Result<serde_json::Value, String>> { Box::pin(async { Err("execute_bash is not implemented for this session host".to_owned()) }) }
    fn set_model(&self, _provider: String, _id: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("set_model is not implemented for this session host".to_owned()) }) }
    fn set_session_name(&self, _name: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("set_session_name is not implemented for this session host".to_owned()) }) }
    fn set_session_thinking_level(&self, _level: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Err("set_session_thinking_level is not implemented for this session host".to_owned()) }) }
    fn cycle_thinking_level(&self) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Err("cycle_thinking_level is not implemented for this session host".to_owned()) }) }
    fn cycle_model(&self, _forward: bool) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Err("cycle_model is not implemented for this session host".to_owned()) }) }
    /// Fetch the host's session stats, generation-tagged, to `sender`. A no-op locally.
    fn request_remote_stats(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<serde_json::Value, String>)>) {}
    fn export_jsonl(&self, _output_path: Option<String>) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Err("export_jsonl is not implemented for this session host".to_owned()) }) }
    fn new_session(&self, parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>>;
    fn switch_session(&self, session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>>;
    fn fork(&self, entry_id: String, include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>>;
    fn dispose(&self) -> SessionFuture<'_, ()>;
}

pub struct LocalInteractiveSession {
    session: Arc<AgentSession>,
}

impl LocalInteractiveSession {
    pub fn new(session: Arc<AgentSession>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Arc<AgentSession> {
        &self.session
    }
}

impl InteractiveSession for LocalInteractiveSession {
    fn session_id(&self) -> Option<String> {
        Some(self.session.session_id())
    }

    fn session_file(&self) -> Option<String> {
        self.session.session_file()
    }

    fn cwd(&self) -> String {
        self.session.cwd()
    }

    fn is_reconnecting(&self) -> bool {
        false
    }

    fn is_fallback(&self) -> bool {
        false
    }

    fn remote_state(&self) -> Option<crate::interactive_host_runtime::RemoteSessionState> {
        None
    }

    fn subscribe_session_events(&self, _listener: crate::interactive_host_runtime::SessionEventListener) -> Option<crate::interactive_host_runtime::SessionSubscription> {
        None
    }

    fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}

    fn new_session(&self, parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
        let options = Some(NewSessionOptions { id: None, parent_session });
        Box::pin(async move {
            self.session
                .new_session(options)
                .await
                .map(|replaced| if replaced { ReplacementOutcome::Replaced } else { ReplacementOutcome::Cancelled })
        })
    }

    fn switch_session(&self, session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
        Box::pin(async move {
            self.session
                .switch_session(&session_path)
                .await
                .map(|replaced| if replaced { ReplacementOutcome::Replaced } else { ReplacementOutcome::Cancelled })
        })
    }

    fn fork(&self, entry_id: String, include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> {
        Box::pin(async move {
            self.session.fork(&entry_id, include_entry).await.map(|result| ForkOutcome {
                outcome: if result.cancelled { ReplacementOutcome::Cancelled } else { ReplacementOutcome::Replaced },
                editor_text: result.editor_text,
            })
        })
    }

    fn dispose(&self) -> SessionFuture<'_, ()> {
        Box::pin(async move { self.session.dispose().await })
    }

    fn prompt(&self, message: String, options: maho_core::agent_session::PromptOptions) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.prompt(&message, options).await.map(|_| ()) })
    }

    fn abort(&self) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.abort().await; Ok(()) })
    }

    fn steer(&self, text: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.steer(&text, None, Default::default()).await })
    }

    fn follow_up(&self, text: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.follow_up(&text, None, Default::default()).await })
    }

    fn compact(&self, instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.compact(instructions.as_deref()).await.map(|_| ()) })
    }

    fn navigate_tree(&self, entry_id: String, options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> {
        Box::pin(async move { self.session.navigate_tree(&entry_id, options).await })
    }

    fn edit_assistant_message(&self, entry_id: String, text: String, options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> {
        Box::pin(async move { self.session.edit_assistant_message(&entry_id, &text, options).await })
    }

    fn reload(&self) -> SessionFuture<'_, Result<bool, String>> {
        Box::pin(async move { self.session.reload().await })
    }

    fn execute_bash(&self, command: String, exclude_from_context: bool) -> SessionFuture<'_, Result<serde_json::Value, String>> {
        Box::pin(async move { self.session.execute_bash(&command, None, exclude_from_context, None, None).await.map(|result| serde_json::to_value(result).unwrap_or(serde_json::Value::Null)) })
    }

    fn set_model(&self, provider: String, id: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let model = self.session.model_registry().find(&provider, &id).ok_or_else(|| format!("Model not found: {provider}/{id}"))?;
            self.session.set_model(model).await.map(|_| ())
        })
    }

    fn set_session_name(&self, name: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.set_session_name(&name); Ok(()) })
    }

    fn set_session_thinking_level(&self, level: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { self.session.set_session_thinking_level(maho_ai::types::ModelThinkingLevel::parse(&level).unwrap_or(maho_ai::types::ModelThinkingLevel::Off)); Ok(()) })
    }

    fn cycle_thinking_level(&self) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move { Ok(self.session.cycle_thinking_level().map(|level| level.as_str().to_owned())) })
    }

    fn cycle_model(&self, forward: bool) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move { Ok(self.session.cycle_model(forward).await?.map(|result| result.model.name)) })
    }

    fn export_jsonl(&self, output_path: Option<String>) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move { self.session.export_to_jsonl(output_path.as_deref()).map(Some).map_err(|error| error.to_string()) })
    }
}

fn remote_outcome(
    runtime: &RemoteInteractiveRuntime,
    result: Result<bool, maho_rpc::rpc_client::RpcClientError>,
) -> Result<ReplacementOutcome, String> {
    match result {
        Ok(true) if runtime.is_fallback() => Ok(ReplacementOutcome::LocalHandoff),
        Ok(true) => Ok(ReplacementOutcome::Cancelled),
        Ok(false) => Ok(ReplacementOutcome::Replaced),
        Err(error) => Err(error.to_string()),
    }
}

impl InteractiveSession for RemoteInteractiveRuntime {
    fn session_id(&self) -> Option<String> {
        self.proxy().session_id()
    }

    fn session_file(&self) -> Option<String> {
        self.proxy().session_file()
    }

    fn cwd(&self) -> String {
        self.binding().cwd.clone()
    }

    fn is_reconnecting(&self) -> bool {
        RemoteInteractiveRuntime::is_reconnecting(self)
    }

    fn is_fallback(&self) -> bool {
        RemoteInteractiveRuntime::is_fallback(self)
    }

    fn remote_state(&self) -> Option<crate::interactive_host_runtime::RemoteSessionState> {
        Some(self.proxy().state())
    }

    fn subscribe_session_events(&self, listener: crate::interactive_host_runtime::SessionEventListener) -> Option<crate::interactive_host_runtime::SessionSubscription> {
        Some(self.proxy().subscribe_session_events(listener))
    }

    fn request_remote_history(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {
        self.proxy().spawn_fetch_messages(generation, sender);
    }

    fn request_remote_models(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {
        self.proxy().spawn_fetch_models(generation, sender);
    }

    fn prompt(&self, message: String, options: maho_core::agent_session::PromptOptions) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::prompt(self, &message, prompt_options_to_wire(&options)).await.map_err(|error| error.to_string()) })
    }

    fn abort(&self) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::abort(self).await.map_err(|error| error.to_string()) })
    }

    fn steer(&self, text: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::steer(self, &text, None, None).await.map_err(|error| error.to_string()) })
    }

    fn follow_up(&self, text: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::follow_up(self, &text, None, None).await.map_err(|error| error.to_string()) })
    }

    fn compact(&self, instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::compact(self, instructions.as_deref()).await.map(|_| ()).map_err(|error| error.to_string()) })
    }

    fn navigate_tree(&self, entry_id: String, options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> {
        Box::pin(async move { Ok(assistant_edit_result_from_wire(&RemoteInteractiveRuntime::navigate_tree(self, &entry_id, tree_options_to_wire(&options)).await.map_err(|error| error.to_string())?)) })
    }

    fn edit_assistant_message(&self, entry_id: String, text: String, options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> {
        Box::pin(async move { Ok(assistant_edit_result_from_wire(&RemoteInteractiveRuntime::edit_assistant_message(self, &entry_id, &text, tree_options_to_wire(&options)).await.map_err(|error| error.to_string())?)) })
    }

    fn reload(&self) -> SessionFuture<'_, Result<bool, String>> {
        Box::pin(async move { RemoteInteractiveRuntime::reload(self).await.map(|value| value.get("cancelled").and_then(serde_json::Value::as_bool) != Some(true)).map_err(|error| error.to_string()) })
    }

    fn fire_clear_queue(&self, abort_will_follow: bool) {
        self.proxy().spawn_clear_queue(abort_will_follow);
    }

    fn execute_bash(&self, command: String, exclude_from_context: bool) -> SessionFuture<'_, Result<serde_json::Value, String>> {
        Box::pin(async move { RemoteInteractiveRuntime::bash(self, &command, serde_json::json!({"excludeFromContext": exclude_from_context})).await.map_err(|error| error.to_string()) })
    }

    fn set_model(&self, provider: String, id: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::set_model(self, &provider, &id).await.map(|_| ()).map_err(|error| error.to_string()) })
    }

    fn set_session_name(&self, name: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::set_session_name(self, &name).await.map_err(|error| error.to_string()) })
    }

    fn set_session_thinking_level(&self, level: String) -> SessionFuture<'_, Result<(), String>> {
        Box::pin(async move { RemoteInteractiveRuntime::set_thinking_level(self, &level, Some("turn")).await.map(|_| ()).map_err(|error| error.to_string()) })
    }

    fn cycle_thinking_level(&self) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move { Ok(RemoteInteractiveRuntime::cycle_thinking_level(self).await.map_err(|error| error.to_string())?.get("level").and_then(serde_json::Value::as_str).map(str::to_owned)) })
    }

    fn cycle_model(&self, forward: bool) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            let value = RemoteInteractiveRuntime::cycle_model(self, if forward { "forward" } else { "backward" }).await.map_err(|error| error.to_string())?;
            Ok(value.get("model").and_then(|model| model.get("name")).and_then(serde_json::Value::as_str).map(str::to_owned))
        })
    }

    fn request_remote_stats(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<serde_json::Value, String>)>) {
        self.proxy().spawn_fetch_stats(generation, sender);
    }

    fn export_jsonl(&self, output_path: Option<String>) -> SessionFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move { Ok(RemoteInteractiveRuntime::export_jsonl(self, output_path.as_deref()).await.map_err(|error| error.to_string())?.get("path").and_then(serde_json::Value::as_str).map(str::to_owned)) })
    }

    fn new_session(&self, parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
        Box::pin(async move { remote_outcome(self, RemoteInteractiveRuntime::new_session(self, parent_session.as_deref()).await) })
    }

    fn switch_session(&self, session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
        Box::pin(async move { remote_outcome(self, RemoteInteractiveRuntime::switch_session(self, &session_path, None).await) })
    }

    fn fork(&self, entry_id: String, include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> {
        Box::pin(async move {
            let position = Some(if include_entry { "at" } else { "before" });
            let outcome = remote_outcome(self, RemoteInteractiveRuntime::fork(self, &entry_id, position).await)?;
            Ok(ForkOutcome { outcome, editor_text: None })
        })
    }

    fn dispose(&self) -> SessionFuture<'_, ()> {
        Box::pin(async move { RemoteInteractiveRuntime::dispose(self).await })
    }
}

fn assistant_edit_result_from_wire(value: &serde_json::Value) -> maho_core::agent_session::AssistantEditResult {
    maho_core::agent_session::AssistantEditResult {
        editor_text: value.get("editorText").and_then(serde_json::Value::as_str).map(str::to_owned),
        cancelled: value.get("cancelled").and_then(serde_json::Value::as_bool).unwrap_or(false) || value.get("outcome").and_then(serde_json::Value::as_str) == Some("cancelled"),
        aborted: value.get("aborted").and_then(serde_json::Value::as_bool),
        summary_entry: value.get("summaryEntry").cloned(),
        unchanged: value.get("outcome").and_then(serde_json::Value::as_str).map(|outcome| outcome == "unchanged"),
        entry_id: value.get("entry").and_then(|entry| entry.get("id")).and_then(serde_json::Value::as_str).map(str::to_owned).or_else(|| value.get("entryId").and_then(serde_json::Value::as_str).map(str::to_owned)),
    }
}

fn prompt_options_to_wire(options: &maho_core::agent_session::PromptOptions) -> serde_json::Value {
    let mut value = serde_json::json!({});
    if let Some(images) = &options.images {
        value["images"] = serde_json::to_value(images).unwrap_or(serde_json::Value::Null);
    }
    if let Some(behavior) = options.streaming_behavior {
        value["streamingBehavior"] = match behavior { maho_ext_api::StreamingBehavior::Steer => "steer", maho_ext_api::StreamingBehavior::FollowUp => "followUp" }.into();
    }
    if let Some(level) = options.thinking_level {
        value["thinkingLevel"] = level.as_str().into();
    }
    if let Some(expand) = options.expand_prompt_templates {
        value["expandPromptTemplates"] = expand.into();
    }
    value
}

fn tree_options_to_wire(options: &maho_core::agent_session::TreeNavigationOptions) -> serde_json::Value {
    let mut value = serde_json::json!({});
    if let Some(summarize) = options.summarize { value["summarize"] = summarize.into(); }
    if let Some(instructions) = &options.custom_instructions { value["customInstructions"] = instructions.clone().into(); }
    if let Some(replace) = options.replace_instructions { value["replaceInstructions"] = replace.into(); }
    if let Some(label) = &options.label { value["label"] = label.clone().into(); }
    if let Some(leaf) = &options.expected_leaf_id { value["expectedLeafId"] = leaf.clone().into(); }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::AgentSessionConfig;
    use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
    use maho_core::session_manager::SessionManager;
    use maho_core::settings_manager::{InMemorySettingsStorage, SettingsManager};

    fn local_session(directory: &tempfile::TempDir) -> Arc<AgentSession> {
        let cwd = directory.path().to_string_lossy().into_owned();
        let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() });
        let model = provider.get_model(Some("faux-1")).expect("model");
        provider.set_responses(vec![faux_assistant_message("hello", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
            let mut context = context.clone();
            context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
            streams.stream_simple(model, &context, options.map(|options| options.simple))
        });
        let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            models_path: Some(directory.path().join("models.json")),
            auth_path: Some(directory.path().join("auth.json")),
            providers: Some(vec![provider.provider.clone()]),
            ..Default::default()
        });
        Arc::new(
            AgentSession::new(AgentSessionConfig {
                agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                    initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
                    stream_fn: Some(stream_fn),
                    ..Default::default()
                }),
                session_manager: SessionManager::in_memory(&cwd, None, None),
                settings_manager: SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false),
                cwd: cwd.clone(),
                agent_dir: Some(cwd),
                fallback_now: Some(Arc::new(|| 0.0)),
                retry_random: Some(Arc::new(|| 0.5)),
                scoped_models: Vec::new(),
                favorite_models: Vec::new(),
                flag_values: Default::default(),
                custom_tools: Vec::new(),
                model_runtime: Some(runtime),
                model_registry: None,
                uses_default_stream_function: Some(false),
                initial_active_tool_names: None,
                default_tool_names: None,
                eval_only_tool_names: None,
                allowed_tool_names: None,
                excluded_tool_names: None,
                base_tools_override: None,
                session_start_event: None,
                auto_title_sessions: Some(false),
            })
            .expect("session"),
        )
    }

    #[tokio::test]
    async fn local_session_reports_identity_and_never_hands_off() {
        let directory = tempfile::tempdir().expect("directory");
        let session = local_session(&directory);
        let local = LocalInteractiveSession::new(session.clone());
        assert!(local.session_id().is_some_and(|id| !id.is_empty()));
        assert_eq!(local.cwd(), session.cwd());
        assert!(!local.is_fallback());
        assert!(!local.is_reconnecting());
        let invalid = directory.path().join("missing.jsonl");
        assert!(local.switch_session(invalid.to_string_lossy().into_owned()).await.is_err());
        local.dispose().await;
    }
}
