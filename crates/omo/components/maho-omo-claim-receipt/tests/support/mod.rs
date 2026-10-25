#![allow(dead_code)]
//! Fixture entries and a dispatch harness mirroring upstream `entries.test-support.ts` and the
//! `FakeExtensionAPI` setup of `index.test.ts`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_ext_api::*;
use maho_omo_claim_receipt::index::{ClaimReceiptComponent, CLAIM_RECEIPT_NUDGE_FLAG};
use serde_json::{json, Value};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn next_id() -> String {
    format!("e{}", SEQUENCE.fetch_add(1, Ordering::SeqCst) + 1)
}

pub fn user_input(text: &str) -> Value {
    json!({
        "type": "message",
        "id": next_id(),
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
    })
}

pub fn answer(text: &str) -> Value {
    json!({
        "type": "message",
        "id": next_id(),
        "message": { "role": "assistant", "stopReason": "stop", "content": [{ "type": "text", "text": text }] }
    })
}

pub fn tool_call(call_id: &str, name: &str, args: Value) -> Value {
    json!({
        "type": "message",
        "id": next_id(),
        "message": {
            "role": "assistant",
            "stopReason": "toolUse",
            "content": [{ "type": "toolCall", "id": call_id, "name": name, "arguments": args }]
        }
    })
}

pub fn tool_result(call_id: &str, tool_name: &str, text: &str, details: Value, is_error: bool) -> Value {
    json!({
        "type": "message",
        "id": next_id(),
        "message": {
            "role": "toolResult",
            "toolCallId": call_id,
            "toolName": tool_name,
            "content": [{ "type": "text", "text": text }],
            "details": details,
            "isError": is_error
        }
    })
}

pub struct CellFixture<'a> {
    pub code: &'a str,
    pub output: &'a str,
    /// `None` is the fixture's default `complete` status.
    pub status: Option<&'a str>,
}

pub fn eval_run(call_id: &str, cell: &CellFixture<'_>, host_calls: Vec<Value>) -> Vec<Value> {
    let status = cell.status.unwrap_or("complete");
    vec![
        tool_call(call_id, "eval", json!({ "language": "js", "code": cell.code })),
        tool_result(
            call_id,
            "eval",
            cell.output,
            json!({
                "cells": [{ "index": 0, "code": cell.code, "output": cell.output, "status": status }],
                "toolCalls": host_calls
            }),
            cell.status == Some("error"),
        ),
    ]
}

pub fn host_bash(command: &str, result_preview: &str, ok: bool, extra_args: Value) -> Value {
    let mut args = json!({ "command": command });
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra_args.as_object()) {
        for (key, value) in extra {
            args.insert(key.clone(), value.clone());
        }
    }
    json!({ "name": "bash", "ok": ok, "args": args, "resultPreview": result_preview })
}

pub fn custom_message(custom_type: &str, content: &str, details: Value) -> Value {
    json!({
        "type": "custom_message",
        "id": next_id(),
        "customType": custom_type,
        "content": content,
        "display": false,
        "details": details
    })
}

pub fn edit_result(call_id: &str, path: &str) -> Vec<Value> {
    vec![
        tool_call(call_id, "edit", json!({ "path": path, "edits": [] })),
        tool_result(call_id, "edit", "ok", json!({}), false),
    ]
}

pub fn passing_test(call_id: &str) -> Vec<Value> {
    eval_run(
        call_id,
        &CellFixture {
            code: "await sh(`bun test src`)",
            output: " 12 pass\n 0 fail",
            status: Some("complete"),
        },
        Vec::new(),
    )
}

pub fn failing_test(call_id: &str) -> Vec<Value> {
    eval_run(
        call_id,
        &CellFixture {
            code: "await tool.bash({ command: 'bun test src' })",
            output: "",
            status: Some("complete"),
        },
        vec![host_bash("bun test src", " 11 pass\n 1 fail", true, json!({}))],
    )
}

/// A branch that the harness can rewrite between dispatches.
#[derive(Clone)]
pub struct Branch(Arc<Mutex<Vec<Value>>>);

impl Branch {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    pub fn set(&self, entries: Vec<Value>) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = entries;
    }
}

impl Default for Branch {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TestSession {
    pub session_id: String,
    pub branch: Branch,
}

impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.get_branch()
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        let entries = self
            .branch
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        entries
            .iter()
            .enumerate()
            .map(|(index, value)| SessionEntry {
                id: value["id"].as_str().map_or_else(|| format!("id{index}"), str::to_owned),
                parent_id: None,
                timestamp: "2026-01-01T00:00:00.000Z".to_owned(),
                kind: value["type"].as_str().unwrap_or("message").to_owned(),
                data: value.clone(),
            })
            .collect()
    }

    fn get_leaf_id(&self) -> Option<String> {
        None
    }

    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestRegistry;

impl ModelRegistry for TestRegistry {
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

    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestUi;

impl ExtensionUi for TestUi {
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
        Box::pin(async { Err("UI not available".into()) })
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }
}

#[derive(Default)]
pub struct RecordingLogger {
    entries: Mutex<Vec<(String, String, Option<JsonValue>)>>,
}

impl RecordingLogger {
    pub fn messages(&self, level: &str) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(recorded, _, _)| recorded == level)
            .map(|(_, message, _)| message.clone())
            .collect()
    }
}

impl ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, details: Option<&JsonValue>) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(("info".to_owned(), message.to_owned(), details.cloned()));
    }

    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(("warn".to_owned(), message.to_owned(), details.cloned()));
    }

    fn error(&self, message: &str, details: Option<&JsonValue>) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(("error".to_owned(), message.to_owned(), details.cloned()));
    }
}

pub fn context(session: TestSession, logger: Option<Arc<RecordingLogger>>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(session),
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
        logger: logger.map(|logger| logger as Arc<dyn ComponentLogger>),
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

/// The only assistant shape `read_agent_end_outcome` lets continue.
pub fn finished_turn() -> Vec<AgentMessage> {
    vec![
        serde_json::from_value(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "done" }],
            "api": "faux",
            "provider": "faux",
            "model": "faux",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": 0
        }))
        .expect("finished assistant message"),
    ]
}

pub async fn dispatch(
    api: &ExtensionApi,
    kind: EventKind,
    event: &mut ExtensionEvent,
    ctx: &ExtensionContext,
) -> EventResult {
    let handler = api
        .registered
        .handlers
        .get(&kind)
        .and_then(|handlers| handlers.first())
        .cloned()
        .unwrap_or_else(|| panic!("no handler registered for {}", kind.as_str()));
    handler(event, ctx).await.expect("handler dispatch")
}

/// What `agent_before_settle` returned: the appended drafts and the continuation request.
pub struct Settle {
    pub entries: Option<Vec<Value>>,
    pub should_continue: bool,
}

pub struct Harness {
    pub api: ExtensionApi,
    pub branch: Branch,
    pub ctx: ExtensionContext,
    pub logger: Arc<RecordingLogger>,
}

impl Harness {
    pub fn new(env: &[(&str, &str)], nudge: bool) -> Self {
        let mut api = ExtensionApi::new(
            LoadedExtension::new("claim-receipt", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        let env: BTreeMap<String, String> = env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        ClaimReceiptComponent::new(env).register(&mut api);
        if nudge {
            api.set_flag(CLAIM_RECEIPT_NUDGE_FLAG, FlagValue::Boolean(true));
        }
        let branch = Branch::new();
        let logger = Arc::new(RecordingLogger::default());
        let ctx = context(
            TestSession {
                session_id: "s1".to_owned(),
                branch: branch.clone(),
            },
            Some(Arc::clone(&logger)),
        );
        Self {
            api,
            branch,
            ctx,
            logger,
        }
    }

    pub async fn input(&self, source: InputSource) {
        let mut event = ExtensionEvent::Input(InputEvent {
            input_id: "id".to_owned(),
            text: "go".to_owned(),
            images: None,
            source,
            streaming_behavior: None,
        });
        dispatch(&self.api, EventKind::Input, &mut event, &self.ctx).await;
    }

    /// Dispatches `agent_end` then `agent_before_settle` and returns the drafts the settle appended.
    pub async fn finish(&self, entries: Vec<Value>) -> Option<Vec<Value>> {
        self.finish_run(entries, None, None).await.entries
    }

    pub async fn finish_run(
        &self,
        entries: Vec<Value>,
        aborted: Option<bool>,
        will_retry: Option<bool>,
    ) -> Settle {
        self.branch.set(entries);
        let mut end = ExtensionEvent::AgentEnd {
            messages: finished_turn(),
            aborted,
            will_retry,
            abort_source: None,
        };
        dispatch(&self.api, EventKind::AgentEnd, &mut end, &self.ctx).await;
        let mut settle = ExtensionEvent::AgentBeforeSettle {
            entries: vec![json!({ "type": "custom", "customType": "other" })],
            should_continue: false,
            context: json!({ "outcome": "completed" }),
        };
        dispatch(&self.api, EventKind::AgentBeforeSettle, &mut settle, &self.ctx).await;
        match settle {
            ExtensionEvent::AgentBeforeSettle {
                entries,
                should_continue,
                ..
            } => Settle {
                entries: (entries.len() > 1).then_some(entries),
                should_continue,
            },
            _ => Settle {
                entries: None,
                should_continue: false,
            },
        }
    }

    pub async fn tool_call(&self, input: Value) -> Value {
        let mut event = ExtensionEvent::ToolCall(ToolCallEvent {
            tool_call_id: "call".to_owned(),
            tool_name: "task".to_owned(),
            input,
        });
        dispatch(&self.api, EventKind::ToolCall, &mut event, &self.ctx).await;
        match event {
            ExtensionEvent::ToolCall(call) => call.input,
            _ => Value::Null,
        }
    }
}

/// The drafts' `customType` sequence, the shape most upstream assertions use.
pub fn draft_types(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry["customType"].as_str().unwrap_or("").to_owned())
        .collect()
}
