use crate::{
    format::parse_ask_user_answer_frame,
    schema::{AskUserVariant, DEFAULT_ASK_USER_TIMEOUT_MS, to_canonical},
};
use serde_json::Value;
use std::collections::HashSet;
pub struct DanglingQuestion {
    pub tool_call_id: String,
    pub variant: AskUserVariant,
    pub args: Value,
}
pub const ASK_USER_RESUMED_ENTRY: &str = "ask-user:resumed";
pub(crate) async fn resume_dangling_questions(
    sender: std::sync::Arc<maho_ext_api::ExtensionApi>,
    ctx: &maho_ext_api::ExtensionContext,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let entries = ctx
        .session_manager
        .get_branch()
        .into_iter()
        .map(|entry| entry.data)
        .collect::<Vec<_>>();
    let mut pending = crate::registry::get_pending_questions(ctx.session_manager.session_id())
        .into_iter()
        .map(|entry| entry.request.request_id.clone())
        .collect::<HashSet<_>>();
    let timeout = (ctx.get_ask_user_settings()?.timeout_minutes * 60_000.0) as u64;
    for dangling in find_dangling_questions(&entries) {
        if !pending.insert(dangling.tool_call_id.clone()) {
            continue;
        }
        sender.append_entry(
            ASK_USER_RESUMED_ENTRY,
            Some(serde_json::json!({"toolCallId":dangling.tool_call_id})),
        )?;
        let required = dangling.args.get("required").and_then(Value::as_bool) == Some(true);
        let Ok(request) = to_canonical(
            dangling.variant,
            &dangling.args,
            dangling.tool_call_id.clone(),
            Some(timeout),
        ) else {
            settle_unrestorable(&sender, ctx, &dangling, timeout)?;
            continue;
        };
        crate::tool::start_question(
            sender.clone(),
            ctx.clone(),
            request,
            None,
            std::sync::Arc::new(std::sync::Mutex::new(crate::tool::AskUserState::default())),
            crate::tool::QuestionExecution {
                variant: dangling.variant,
                resuming: true,
                required,
            },
        )
        .await?;
    }
    Ok(())
}
/// A call that cannot be restored never reaches the UI: it settles as orphaned-after-restart, like a
/// restored question whose UI is gone, so the model learns it was lost and can ask again.
fn settle_unrestorable(
    sender: &maho_ext_api::ExtensionApi,
    ctx: &maho_ext_api::ExtensionContext,
    dangling: &DanglingQuestion,
    timeout: u64,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let lost = maho_ext_api::QuestionRequest {
        request_id: dangling.tool_call_id.clone(),
        questions: vec![],
        wait_for_answer: false,
        timeout_ms: timeout,
    };
    let response = maho_ext_api::QuestionResponse {
        status: maho_ext_api::QuestionStatus::OrphanedAfterRestart,
        answers: Default::default(),
        comment: None,
        unanswered: vec![],
        auto_resolved_after_ms: None,
        cancel_origin: None,
        resolved_by: None,
    };
    sender.append_entry(crate::notify::ASK_USER_SETTLEMENT_ENTRY,Some(serde_json::json!({"requestId":lost.request_id,"status":crate::format::status_name(response.status)})))?;
    crate::notify::emit_closed(&sender.events, &lost.request_id, &response);
    if let Some(text) = crate::tool::answer_delivery(&lost, &response, false) {
        let deliver_as = if ctx.is_idle() {
            maho_ext_api::StreamingBehavior::FollowUp
        } else {
            maho_ext_api::StreamingBehavior::Steer
        };
        sender.send_user_message(
            maho_ext_api::UserMessageContent::Text(text),
            maho_ext_api::SendUserMessageOptions {
                deliver_as: Some(deliver_as),
                expand_prompt_templates: false,
            },
        )?;
    }
    crate::notify::emit_notification(&sender.events, ctx, &lost, &response, dangling.variant);
    Ok(())
}
pub fn find_dangling_questions(entries: &[Value]) -> Vec<DanglingQuestion> {
    let mut results = HashSet::new();
    let mut accepted = HashSet::new();
    let mut resumed = HashSet::new();
    let mut settled = HashSet::new();
    for entry in entries {
        if entry["type"] == "message" && entry["message"]["role"] == "user" {
            let content = &entry["message"]["content"];
            let texts = if let Some(text) = content.as_str() {
                vec![text]
            } else {
                content.as_array().map_or(Vec::new(), |parts| {
                    parts
                        .iter()
                        .filter(|part| part["type"] == "text")
                        .filter_map(|part| part["text"].as_str())
                        .collect()
                })
            };
            for text in texts {
                if let Some((id, _)) = parse_ask_user_answer_frame(text) {
                    settled.insert(id.to_owned());
                }
            }
        }
        if entry["type"] == "custom"
            && entry["customType"] == "ask-user:resumed"
            && let Some(id) = entry["data"]["toolCallId"].as_str()
        {
            resumed.insert(id.to_owned());
        }
        if entry["type"] == "custom"
            && entry["customType"] == "ask-user:settlement"
            && let Some(id) = entry["data"]["requestId"].as_str()
        {
            settled.insert(id.to_owned());
        }
        if entry["type"] == "message"
            && entry["message"]["role"] == "toolResult"
            && let Some(id) = entry["message"]["toolCallId"].as_str()
        {
            results.insert(id.to_owned());
            let message = &entry["message"];
            if message["isError"] != true
                && message["details"]["accepted"] == true
                && message["details"]["status"] == "pending"
            {
                accepted.insert(id.to_owned());
            }
        }
    }
    let mut dangling = Vec::new();
    for entry in entries.iter().rev() {
        if entry["type"] != "message" || entry["message"]["role"] != "assistant" {
            continue;
        }
        let Some(blocks) = entry["message"]["content"].as_array() else {
            continue;
        };
        for block in blocks.iter().rev() {
            if block["type"] != "toolCall" || block["incomplete"] == true {
                continue;
            }
            let variant = match block["name"].as_str() {
                Some("request_user_input") => AskUserVariant::Codex,
                Some("ask_user_question") => AskUserVariant::Claude,
                _ => continue,
            };
            let Some(id) = block["id"].as_str() else {
                continue;
            };
            if settled.contains(id) || resumed.contains(id) {
                continue;
            }
            // Arguments that no longer form a valid question set still dangle: resume settles them
            // orphaned-after-restart instead of rendering them (omo #9268).
            let wait = to_canonical(
                variant,
                &block["arguments"],
                id.into(),
                Some(DEFAULT_ASK_USER_TIMEOUT_MS),
            )
            .ok()
            .map(|request| request.wait_for_answer);
            if results.contains(id) && (wait == Some(true) || !accepted.contains(id)) {
                continue;
            }
            dangling.push(DanglingQuestion {
                tool_call_id: id.into(),
                variant,
                args: block["arguments"].clone(),
            });
        }
    }
    dangling
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::*;
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct Capture {
        entries: Mutex<Vec<(String, Option<Value>)>>,
        messages: Mutex<Vec<(String, Option<StreamingBehavior>)>>,
    }
    impl ExtensionActions for Capture {
        fn send_message(
            &self,
            _: CustomMessage,
            _: SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Err("unexpected custom message".into())
        }
        fn send_user_message(
            &self,
            content: UserMessageContent,
            options: SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            let UserMessageContent::Text(text) = content else {
                return Err("unexpected blocks".into());
            };
            self.messages
                .lock()
                .expect("messages")
                .push((text, options.deliver_as));
            Ok(())
        }
        fn append_entry(&self, kind: &str, data: Option<Value>) -> Result<(), ExtensionFailure> {
            self.entries
                .lock()
                .expect("entries")
                .push((kind.into(), data));
            Ok(())
        }
        fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
            Ok(Vec::new())
        }
    }
    struct Session;
    impl ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "s"
        }
        fn session_file(&self) -> Option<&Path> {
            None
        }
    }
    impl SessionManager for Session {
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
    struct Registry;
    impl ModelRegistry for Registry {
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
    struct Ui;
    impl ExtensionUi for Ui {
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
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, JsonValue> {
            Box::pin(async { Err("UI unavailable".into()) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }
    fn context() -> ExtensionContext {
        ExtensionContext {
            ui: Arc::new(Ui),
            mode: ExtensionMode::Tui,
            has_ui: true,
            cwd: "/tmp".into(),
            agent_dir: "/tmp/agent".into(),
            session_manager: Arc::new(Session),
            model_registry: Arc::new(Registry),
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
            get_system_prompt_fn: Arc::new(String::new),
            get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
            registered_mcp_servers: Vec::new(),
            update_tool_hook_status: None,
            idle_coordinator: None,
            logger: None,
            defer_macrotask: None,
            compaction_signal: Default::default(),
        }
    }
    // omo #9268 / senpi resume.ts settleUnrestorable: a dangling call whose arguments no longer form a
    // question set settles orphaned-after-restart once (settlement entry, closed event, one framed
    // follow-up) and never reaches the question UI.
    #[test]
    fn unrestorable_call_settles_orphaned_without_rendering() {
        let capture = Arc::new(Capture::default());
        let runtime = ExtensionRuntime::default();
        runtime.bind(capture.clone());
        let api = ExtensionApi::new(
            LoadedExtension::new("ask-user", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        let closed = Arc::new(Mutex::new(Vec::new()));
        let captured = closed.clone();
        let _closed = api.events.on(
            crate::notify::ASK_USER_CLOSED_EVENT,
            Arc::new(move |value| captured.lock().expect("closed").push(value.clone())),
        );
        for args in [
            serde_json::json!({}),
            serde_json::json!({"questions":[],"waitForAnswer":false}),
        ] {
            let dangling = DanglingQuestion {
                tool_call_id: "call".into(),
                variant: AskUserVariant::Claude,
                args,
            };
            assert!(
                to_canonical(
                    dangling.variant,
                    &dangling.args,
                    "call".into(),
                    Some(60_000)
                )
                .is_err()
            );
            settle_unrestorable(&api, &context(), &dangling, 60_000).expect("settled");
        }
        assert_eq!(
            capture.entries.lock().expect("entries").clone(),
            vec![
                (
                    crate::notify::ASK_USER_SETTLEMENT_ENTRY.to_owned(),
                    Some(serde_json::json!({"requestId":"call","status":"orphaned-after-restart"}))
                );
                2
            ]
        );
        assert_eq!(
            capture.messages.lock().expect("messages").clone(),
            vec![
                (
                    "[Answer to question call]\nThe pending question could not be resumed after a restart; continue on best judgment.".to_owned(),
                    Some(StreamingBehavior::FollowUp)
                );
                2
            ]
        );
        assert_eq!(
            closed.lock().expect("closed").clone(),
            vec![serde_json::json!({"requestId":"call","status":"orphaned-after-restart"}); 2]
        );
    }
}
