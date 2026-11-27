//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-loop-hooks.ts`.

use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use maho_agent::types::AgentMessage;
use maho_ai::model::Model;
use maho_ai::types::{BoxFuture, ImageContent};
use maho_ext_api::{
    BeforeAgentStartEvent, BuildSystemPromptOptions, ExtensionError, ExtensionEvent, InputEvent,
    InputSource, SessionReason, SessionStartEvent, StreamingBehavior,
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::loop_hook_call::{CarriedSession, LoopHookCall, LoopHookCallBinding, LoopHookEffects};
use crate::loopless_tool_handlers::{LooplessExtensionRunner, TwoSidedHome};

/// The loop events a hand that is its extensions' home runs for a loop that runs elsewhere
/// (S33, C8/K5).
pub type LooplessLoopHooks =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

pub const HOOKS_BUILTIN_PATH: &str = "<builtin:hooks>";

/// The user's loop hooks, which a hand runs for a hosted loop: its non-builtin extensions and
/// `<builtin:hooks>`.
pub fn is_user_loop_hook(extension_path: &str) -> bool {
    !extension_path.starts_with("<builtin:") || extension_path == HOOKS_BUILTIN_PATH
}

fn parse_images(value: Option<&Value>) -> Result<Option<Vec<ImageContent>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(arr) = value.as_array() else {
        return Err("images".to_owned());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(obj) = item.as_object() else {
            return Err("images".to_owned());
        };
        if obj.get("type").and_then(Value::as_str) != Some("image") {
            return Err("images".to_owned());
        }
        let Some(data) = obj.get("data").and_then(Value::as_str) else {
            return Err("images".to_owned());
        };
        let Some(mime_type) = obj.get("mimeType").and_then(Value::as_str) else {
            return Err("images".to_owned());
        };
        out.push(ImageContent {
            data: data.to_owned(),
            mime_type: mime_type.to_owned(),
        });
    }
    Ok(Some(out))
}

fn parse_input_source(s: &str) -> Option<InputSource> {
    match s {
        "interactive" => Some(InputSource::Interactive),
        "rpc" => Some(InputSource::Rpc),
        "extension" => Some(InputSource::Extension),
        _ => None,
    }
}

fn parse_streaming_behavior(s: &str) -> Option<StreamingBehavior> {
    match s {
        "steer" => Some(StreamingBehavior::Steer),
        "followUp" => Some(StreamingBehavior::FollowUp),
        _ => None,
    }
}

fn parse_session_start_reason(s: &str) -> Option<SessionReason> {
    match s {
        "startup" => Some(SessionReason::Startup),
        "reload" => Some(SessionReason::Reload),
        "new" => Some(SessionReason::New),
        "resume" => Some(SessionReason::Resume),
        "fork" => Some(SessionReason::Fork),
        _ => None,
    }
}

fn parse_carried_session(obj: &Map<String, Value>) -> Option<CarriedSession> {
    let leaf_val = obj.get("leafId")?;
    let leaf_id = if leaf_val.is_null() {
        None
    } else {
        Some(leaf_val.as_str()?.to_owned())
    };
    let entries_arr = obj.get("handEntries")?.as_array()?;
    let mut hand_entries = Vec::with_capacity(entries_arr.len());
    for entry in entries_arr {
        let e_obj = entry.as_object()?;
        if e_obj.get("type").and_then(Value::as_str) != Some("custom")
            || e_obj.get("customType").and_then(Value::as_str).is_none()
            || e_obj.get("id").and_then(Value::as_str).is_none()
        {
            return None;
        }
        hand_entries.push(entry.clone());
    }
    Some(CarriedSession {
        leaf_id,
        hand_entries,
    })
}

fn parse_hook_request_input(value: Option<&Value>) -> Result<(Value, Option<Model>), String> {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Err("hook input is not an object with a payload".to_owned());
    };
    let Some(payload) = obj.get("payload") else {
        return Err("hook input is not an object with a payload".to_owned());
    };
    for key in obj.keys() {
        if key != "payload" && key != "model" {
            return Err(format!("hook input has unknown members: {key}"));
        }
    }
    let model = match obj.get("model") {
        None => None,
        Some(model_val) => {
            let Some(m_obj) = model_val.as_object() else {
                return Err("hook input model is not a model".to_owned());
            };
            if m_obj.contains_key("headers") {
                return Err("hook input model carries headers".to_owned());
            }
            for field in ["id", "name", "api", "provider", "baseUrl"] {
                if m_obj.get(field).and_then(Value::as_str).is_none() {
                    return Err("hook input model is not a model".to_owned());
                }
            }
            Some(
                serde_json::from_value::<Model>(model_val.clone())
                    .map_err(|e| format!("hook input model is not a model: {e}"))?,
            )
        }
    };
    Ok((payload.clone(), model))
}

fn merge_effects(
    result: Value,
    effects: &StdMutex<LoopHookEffects>,
    errors: &[ExtensionError],
) -> Value {
    let mut map = match result {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let eff = effects.lock().unwrap_or_else(PoisonError::into_inner);
    if !eff.sent.is_empty()
        && let Ok(v) = serde_json::to_value(&eff.sent)
    {
        map.insert("sent".to_owned(), v);
    }
    if !eff.notices.is_empty()
        && let Ok(v) = serde_json::to_value(&eff.notices)
    {
        map.insert("notices".to_owned(), v);
    }
    if !eff.entries.is_empty()
        && let Ok(v) = serde_json::to_value(&eff.entries)
    {
        map.insert("entries".to_owned(), v);
    }
    if !eff.user_messages.is_empty()
        && let Ok(v) = serde_json::to_value(&eff.user_messages)
    {
        map.insert("userMessages".to_owned(), v);
    }
    if !errors.is_empty() {
        let errs: Vec<Value> = errors
            .iter()
            .map(|e| {
                json!({
                    "extensionPath": e.extension_path,
                    "event": e.event,
                    "error": e.error,
                })
            })
            .collect();
        map.insert("errors".to_owned(), Value::Array(errs));
    }
    Value::Object(map)
}

pub fn extension_loop_hooks(
    runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    two_sided_home: TwoSidedHome,
    cwd: String,
    call: Arc<LoopHookCall>,
) -> LooplessLoopHooks {
    type ActiveLoopErrors = Arc<StdMutex<Option<(u64, Vec<ExtensionError>)>>>;
    let active_errors: ActiveLoopErrors = Arc::new(StdMutex::new(None));
    let runner_for_listener = Arc::clone(&runner);
    let errors_sink = Arc::clone(&active_errors);
    if let Ok(mut guard) = runner_for_listener.try_lock() {
        guard.on_error(Arc::new(move |err| {
            let mut slot = errors_sink.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((_, list)) = slot.as_mut() {
                list.push(err.clone());
            }
        }));
    }

    Arc::new(move |event, payload| {
        let runner = Arc::clone(&runner);
        let cwd = cwd.clone();
        let call = Arc::clone(&call);
        let active_errors = Arc::clone(&active_errors);
        Box::pin(async move {
            if two_sided_home != TwoSidedHome::Hand {
                return Err("this hand is not the home of its extensions' loop hooks".to_owned());
            }
            let Some(obj) = payload.as_object() else {
                return Err(format!("malformed {event} payload"));
            };
            let Some(session_id) = obj.get("sessionId").and_then(Value::as_str) else {
                return Err(format!("malformed {event} payload"));
            };
            let Some(idle) = obj.get("idle").and_then(Value::as_bool) else {
                return Err(format!("malformed {event} payload"));
            };
            let images = parse_images(obj.get("images"))
                .map_err(|_| format!("malformed {event} payload: images"))?;

            match event.as_str() {
                "input" => {
                    let Some(input_id) = obj.get("inputId").and_then(Value::as_str) else {
                        return Err("malformed input payload".to_owned());
                    };
                    let Some(text) = obj.get("text").and_then(Value::as_str) else {
                        return Err("malformed input payload".to_owned());
                    };
                    let Some(source) = obj
                        .get("source")
                        .and_then(Value::as_str)
                        .and_then(parse_input_source)
                    else {
                        return Err("malformed input payload".to_owned());
                    };
                    let streaming_behavior = match obj.get("streamingBehavior") {
                        None => None,
                        Some(v) => {
                            let Some(parsed) = v.as_str().and_then(parse_streaming_behavior) else {
                                return Err("malformed input payload".to_owned());
                            };
                            Some(parsed)
                        }
                    };
                    let input_event = InputEvent {
                        input_id: input_id.to_owned(),
                        text: text.to_owned(),
                        images,
                        source,
                        streaming_behavior,
                    };
                    let binding = LoopHookCallBinding {
                        session_id: session_id.to_owned(),
                        cwd,
                        idle,
                        carried: None,
                        ends_turn: false,
                    };
                    let call_ref = Arc::clone(&call);
                    call.run(binding, |effects| {
                        let call = Arc::clone(&call_ref);
                        async move {
                            let token = call.current().map(|f| f.token).unwrap_or(0);
                            *active_errors.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some((token, Vec::new()));
                            let res = runner
                                .lock()
                                .await
                                .emit_input(input_event)
                                .await
                                .map_err(|e| e.to_string());
                            let collected_errors = active_errors
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take()
                                .map(|(_, e)| e)
                                .unwrap_or_default();
                            let res = res?;
                            let val = match res {
                                maho_ext_api::InputEventResult::Continue => {
                                    json!({ "action": "continue" })
                                }
                                maho_ext_api::InputEventResult::Handled => {
                                    json!({ "action": "handled" })
                                }
                                maho_ext_api::InputEventResult::Transform { text, images } => {
                                    json!({ "action": "transform", "text": text, "images": images })
                                }
                            };
                            Ok(merge_effects(val, &effects, &collected_errors))
                        }
                    })
                    .await
                }
                "before_agent_start" => {
                    let Some(prompt) = obj.get("prompt").and_then(Value::as_str) else {
                        return Err("malformed before_agent_start payload".to_owned());
                    };
                    let Some(system_prompt) = obj.get("systemPrompt").and_then(Value::as_str)
                    else {
                        return Err("malformed before_agent_start payload".to_owned());
                    };
                    let Some(trigger) = obj.get("trigger").and_then(Value::as_str) else {
                        return Err("malformed before_agent_start payload".to_owned());
                    };
                    if !matches!(trigger, "prompt" | "delivery" | "extension") {
                        return Err("malformed before_agent_start payload".to_owned());
                    }
                    let preview = match obj.get("preview") {
                        None => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => return Err("malformed before_agent_start payload".to_owned()),
                    };
                    if !obj.get("systemPromptOptions").is_some_and(Value::is_object) {
                        return Err("malformed before_agent_start payload".to_owned());
                    }
                    let opts = BuildSystemPromptOptions {
                        cwd: std::path::PathBuf::from(&cwd),
                        ..Default::default()
                    };
                    let event_val = BeforeAgentStartEvent {
                        prompt: prompt.to_owned(),
                        trigger: trigger.to_owned(),
                        preview,
                        images,
                        system_prompt: system_prompt.to_owned(),
                        system_prompt_options: opts,
                    };
                    let binding = LoopHookCallBinding {
                        session_id: session_id.to_owned(),
                        cwd,
                        idle,
                        carried: None,
                        ends_turn: false,
                    };
                    let call_ref = Arc::clone(&call);
                    call.run(binding, |effects| {
                        let call = Arc::clone(&call_ref);
                        async move {
                            let token = call.current().map(|f| f.token).unwrap_or(0);
                            *active_errors.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some((token, Vec::new()));
                            let res = runner
                                .lock()
                                .await
                                .emit_before_agent_start(event_val)
                                .await
                                .map_err(|e| e.to_string());
                            let collected_errors = active_errors
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take()
                                .map(|(_, e)| e)
                                .unwrap_or_default();
                            let combined = res?;
                            let val = match combined {
                                None => json!({}),
                                Some(c) => json!({
                                    "systemPrompt": c.system_prompt,
                                    "messages": c.messages.into_iter().map(|m| json!({
                                        "customType": m.custom_type,
                                        "display": m.display,
                                        "details": m.details,
                                    })).collect::<Vec<_>>(),
                                }),
                            };
                            Ok(merge_effects(val, &effects, &collected_errors))
                        }
                    })
                    .await
                }
                "before_provider_request" => {
                    let (req_payload, model) = parse_hook_request_input(obj.get("input"))?;
                    let model_json = obj.get("input").and_then(|v| v.get("model")).cloned();
                    let binding = LoopHookCallBinding {
                        session_id: session_id.to_owned(),
                        cwd,
                        idle,
                        carried: None,
                        ends_turn: false,
                    };
                    let call_ref = Arc::clone(&call);
                    call.run(binding, |effects| {
                        let call = Arc::clone(&call_ref);
                        async move {
                            let token = call.current().map(|f| f.token).unwrap_or(0);
                            *active_errors.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some((token, Vec::new()));
                            let res = runner
                                .lock()
                                .await
                                .emit_before_provider_request_with_metadata(
                                    req_payload,
                                    model,
                                    None,
                                    None,
                                )
                                .await
                                .map_err(|e| e.to_string());
                            let collected_errors = active_errors
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take()
                                .map(|(_, e)| e)
                                .unwrap_or_default();
                            let out_payload = res?;
                            let output = match model_json {
                                Some(m) => json!({ "payload": out_payload, "model": m }),
                                None => json!({ "payload": out_payload }),
                            };
                            Ok(merge_effects(
                                json!({ "output": output }),
                                &effects,
                                &collected_errors,
                            ))
                        }
                    })
                    .await
                }
                "session_start" => {
                    let Some(reason) = obj
                        .get("reason")
                        .and_then(Value::as_str)
                        .and_then(parse_session_start_reason)
                    else {
                        return Err("malformed session_start payload".to_owned());
                    };
                    let previous_session_file = match obj.get("previousSessionFile") {
                        None => None,
                        Some(v) => Some(
                            v.as_str()
                                .ok_or_else(|| "malformed session_start payload".to_owned())?
                                .to_owned(),
                        ),
                    };
                    let start_event = ExtensionEvent::SessionStart(SessionStartEvent {
                        sizes_only: false,
                        reason,
                        initial_model_provenance: None,
                        previous_session_file,
                        attended: None,
                    });
                    let binding = LoopHookCallBinding {
                        session_id: session_id.to_owned(),
                        cwd,
                        idle,
                        carried: None,
                        ends_turn: false,
                    };
                    let call_ref = Arc::clone(&call);
                    call.run(binding, |effects| {
                        let call = Arc::clone(&call_ref);
                        async move {
                            let token = call.current().map(|f| f.token).unwrap_or(0);
                            *active_errors.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some((token, Vec::new()));
                            let res = runner
                                .lock()
                                .await
                                .emit(start_event)
                                .await
                                .map_err(|e| e.to_string());
                            let collected_errors = active_errors
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take()
                                .map(|(_, e)| e)
                                .unwrap_or_default();
                            res?;
                            Ok(merge_effects(json!({}), &effects, &collected_errors))
                        }
                    })
                    .await
                }
                "agent_end" => {
                    let Some(messages_val) = obj.get("messages").and_then(Value::as_array) else {
                        return Err("malformed agent_end payload".to_owned());
                    };
                    let mut messages: Vec<AgentMessage> = Vec::with_capacity(messages_val.len());
                    for msg in messages_val {
                        if !msg.is_object() || msg.get("role").and_then(Value::as_str).is_none() {
                            return Err("malformed agent_end payload".to_owned());
                        }
                        let parsed: AgentMessage = serde_json::from_value(msg.clone())
                            .map_err(|_| "malformed agent_end payload".to_owned())?;
                        messages.push(parsed);
                    }
                    let aborted = match obj.get("aborted") {
                        None => None,
                        Some(Value::Bool(b)) => Some(*b),
                        Some(_) => return Err("malformed agent_end payload".to_owned()),
                    };
                    let Some(carried) = parse_carried_session(obj) else {
                        return Err("malformed agent_end payload".to_owned());
                    };
                    let agent_end = ExtensionEvent::AgentEnd {
                        messages,
                        aborted,
                        will_retry: None,
                        abort_source: None,
                    };
                    let binding = LoopHookCallBinding {
                        session_id: session_id.to_owned(),
                        cwd,
                        idle,
                        carried: Some(carried),
                        ends_turn: true,
                    };
                    let call_ref = Arc::clone(&call);
                    call.run(binding, |effects| {
                        let call = Arc::clone(&call_ref);
                        async move {
                            let token = call.current().map(|f| f.token).unwrap_or(0);
                            *active_errors.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some((token, Vec::new()));
                            let res = runner
                                .lock()
                                .await
                                .emit(agent_end)
                                .await
                                .map_err(|e| e.to_string());
                            let collected_errors = active_errors
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .take()
                                .map(|(_, e)| e)
                                .unwrap_or_default();
                            res?;
                            Ok(merge_effects(json!({}), &effects, &collected_errors))
                        }
                    })
                    .await
                }
                other => Err(format!("no loop hooks run on a hand for {other}")),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_hook_call::create_loop_hook_call;
    use maho_core::execution::hand_wake_route::late_loop_route;
    use maho_ext_api::{
        ComponentFactory, CustomComponentFactory, CustomUiFactoryOptions, CustomUiOptions,
        ExtensionContext, ExtensionFuture, ExtensionMode, ExtensionRuntime, ExtensionUi,
        ExtensionUiDialogOptions, ExtensionUiFactories, ExtensionWidgetOptions,
        FooterComponentFactory, NotificationType, SessionEntry, SessionManager, Theme,
        ToolSessionManager, TuiComponentFactory, UiFuture, WidgetContent,
    };
    use maho_ext_host::runner::ExtensionRunner;

    struct TestSession;
    impl ToolSessionManager for TestSession {
        fn session_id(&self) -> &str {
            "session"
        }
        fn session_file(&self) -> Option<&std::path::Path> {
            None
        }
    }
    impl SessionManager for TestSession {
        fn get_entries(&self) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_branch(&self) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }
    struct TestRegistry;
    impl maho_ext_api::ModelRegistry for TestRegistry {
        fn get_all(&self) -> Vec<Model> {
            Vec::new()
        }
        fn get_available(&self) -> Vec<Model> {
            Vec::new()
        }
        fn find(&self, _: &str, _: &str) -> Option<Model> {
            None
        }
        fn has_configured_auth(&self, _: &Model) -> bool {
            false
        }
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }
    struct TestUi;
    impl ExtensionUi for TestUi {
        fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
            Some(self)
        }
        fn select<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, bool> {
            Box::pin(async { false })
        }
        fn input<'a>(
            &'a self,
            _: &'a str,
            _: Option<&'a str>,
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn notify(&self, _: &str, _: NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, Value> {
            Box::pin(async { Err("UI not available".into()) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }
    impl ExtensionUiFactories for TestUi {
        fn set_widget_factory(
            &self,
            _: &str,
            _: Option<TuiComponentFactory>,
            _: ExtensionWidgetOptions,
        ) {
        }
        fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
        fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
        fn custom_factory(
            &self,
            _: CustomComponentFactory,
            _: CustomUiFactoryOptions,
        ) -> ExtensionFuture<'_, Value> {
            Box::pin(async { Err("UI not available".into()) })
        }
    }

    fn test_runner() -> ExtensionRunner {
        let context = ExtensionContext {
            ui: Arc::new(TestUi),
            mode: ExtensionMode::Print,
            has_ui: false,
            cwd: "/tmp".into(),
            agent_dir: "/tmp/agent".into(),
            session_manager: Arc::new(TestSession),
            model_registry: Arc::new(TestRegistry),
            model: None,
            thinking_level: None,
            service_tier: None,
            effective_service_tier: None,
            scoped_models: Vec::new(),
            goal_store_file: None,
            loaded_extension_paths: Vec::new(),
            signal: None,
            steering_signal: None,
            is_idle_fn: Arc::new(|| true),
            wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
            is_project_trusted_fn: Arc::new(|| true),
            is_compacting_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(|| "base".into()),
            get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
                cwd: "/tmp".into(),
                ..Default::default()
            }),
            registered_mcp_servers: Vec::new(),
            update_tool_hook_status: None,
            idle_coordinator: None,
            logger: None,
            defer_macrotask: None,
            compaction_signal: Default::default(),
        };
        ExtensionRunner::new(
            Vec::new(),
            ExtensionRuntime::default(),
            Default::default(),
            context,
        )
    }

    #[tokio::test]
    async fn refuses_when_not_hand_home_and_validates_payloads() {
        let runner: Arc<Mutex<dyn LooplessExtensionRunner>> = Arc::new(Mutex::new(test_runner()));
        let call = Arc::new(create_loop_hook_call(late_loop_route(
            None,
            Arc::new(|_| {}),
        )));
        let loop_hooks = extension_loop_hooks(
            Arc::clone(&runner),
            TwoSidedHome::Loop,
            "/tmp".into(),
            Arc::clone(&call),
        );
        let refused = loop_hooks("input".into(), json!({"sessionId": "s", "idle": true}))
            .await
            .expect_err("not hand home");
        assert!(refused.contains("not the home"));

        let hand_hooks = extension_loop_hooks(runner, TwoSidedHome::Hand, "/tmp".into(), call);
        let ok = hand_hooks(
            "session_start".into(),
            json!({"sessionId": "s", "idle": true, "reason": "startup"}),
        )
        .await
        .expect("session_start");
        assert_eq!(ok, json!({}));

        let bad_req = hand_hooks(
            "before_provider_request".into(),
            json!({
                "sessionId": "s",
                "idle": false,
                "input": {
                    "payload": {},
                    "model": {
                        "id": "m", "name": "m", "api": "a", "provider": "p", "baseUrl": "https://x",
                        "headers": { "Authorization": "secret" }
                    }
                }
            }),
        )
        .await
        .expect_err("headers must be refused");
        assert!(bad_req.contains("headers"));
    }
}
