//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-tool-handlers.ts`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use maho_agent::execution::plane::NestedRequestFn;
use maho_ai::types::BoxFuture;
use maho_ext_api::{
    BeforeAgentStartCombinedResult, BeforeAgentStartEvent, EventKind, ExtensionError,
    ExtensionEvent, ExtensionFailure, InputEvent, InputEventResult, Model, RegisteredTool,
    ToolCallEvent, ToolCallEventResult, ToolContent, ToolResultEvent, ToolResultEventResult, Usage,
};
use maho_tools::filesystem_policy::FilesystemPolicyChecker;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::loopless_tool_call::{AfterToolCallArgs, BeforeToolCallArgs, LooplessToolHandlers};

/// Error listener callback compatible with `maho_ext_host::runner::ErrorListener`.
pub type LooplessExtensionErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

/// Trait boundary over `maho_ext_host::runner::ExtensionRunner` so `maho-rpc` avoids a
/// production dependency on `maho-ext-host` (which sits in `dev-dependencies` / downstream
/// `maho-cli`).
#[async_trait]
pub trait LooplessExtensionRunner: Send + Sync {
    fn has_handlers(&self, kind: EventKind) -> bool;

    async fn emit_tool_call(
        &mut self,
        event: &mut ToolCallEvent,
    ) -> Result<Option<ToolCallEventResult>, ExtensionFailure>;

    async fn emit_tool_result(
        &mut self,
        event: ToolResultEvent,
    ) -> Result<Option<ToolResultEventResult>, ExtensionFailure>;

    async fn emit_input(&mut self, input: InputEvent)
    -> Result<InputEventResult, ExtensionFailure>;

    async fn emit_before_agent_start(
        &mut self,
        event: BeforeAgentStartEvent,
    ) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure>;

    async fn emit_before_provider_request_with_metadata(
        &mut self,
        payload: Value,
        model: Option<Model>,
        headers: Option<BTreeMap<String, Option<String>>>,
        exclude_path: Option<&str>,
    ) -> Result<Value, ExtensionFailure>;

    async fn emit(&mut self, event: ExtensionEvent) -> Result<(), ExtensionFailure>;

    fn on_error(&mut self, listener: LooplessExtensionErrorListener);

    fn invalidate(&self, message: &str);

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool>;

    fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker>;
}

/// Where a two-sided extension runs whole: with the loop (fused, split-local; S29) or on the
/// hand (hosted loop; S33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TwoSidedHome {
    #[default]
    Loop,
    Hand,
}

const EXTENSION_EVENTS_PLANE: &str = "extension-events";

struct RunnerToolHandlers {
    runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    two_sided_home: TwoSidedHome,
    two_sided_paths: HashSet<String>,
}

impl RunnerToolHandlers {
    async fn forward_tool_call(
        extension: &str,
        handler: usize,
        event: &mut ToolCallEvent,
        nested: &NestedRequestFn,
    ) -> Result<Option<ToolCallEventResult>, String> {
        let payload = serde_json::json!({
            "toolCallId": event.tool_call_id,
            "toolName": event.tool_name,
            "input": event.input,
            "parentToolCallId": event.parent_tool_call_id,
        });
        let body = serde_json::json!({
            "extension": extension,
            "handler": handler,
            "event": "tool_call",
            "payload": payload,
        });
        let reply = nested(EXTENSION_EVENTS_PLANE, body, None)
            .await
            .map_err(|e| e.to_string())?;
        let Some(obj) = reply.as_object() else {
            return Ok(None);
        };
        if let Some(err) = obj.get("error").and_then(Value::as_str) {
            return Err(err.to_owned());
        }
        if let Some(input) = obj.get("input") {
            event.input = input.clone();
        }
        let Some(res) = obj.get("result").and_then(Value::as_object) else {
            return Ok(None);
        };
        Ok(Some(ToolCallEventResult {
            block: res.get("block").and_then(Value::as_bool),
            reason: res.get("reason").and_then(Value::as_str).map(str::to_owned),
            terminate: res.get("terminate").and_then(Value::as_bool),
        }))
    }
}

impl LooplessToolHandlers for RunnerToolHandlers {
    fn before_tool_call<'a>(
        &'a self,
        event: BeforeToolCallArgs<'a>,
    ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
        Box::pin(async move {
            let mut runner = self.runner.lock().await;
            if !runner.has_handlers(EventKind::ToolCall) {
                return Ok(None);
            }
            let mut call_event = ToolCallEvent {
                tool_call_id: event.tool_call_id.to_owned(),
                tool_name: event.tool_name.to_owned(),
                input: event.input.clone(),
                parent_tool_call_id: None,
            };
            if self.two_sided_home == TwoSidedHome::Loop
                && let Some(nested) = event.nested.as_ref()
                && !self.two_sided_paths.is_empty()
            {
                for ext in &self.two_sided_paths {
                    if let Some(res) =
                        Self::forward_tool_call(ext, 0, &mut call_event, nested).await?
                        && res.block == Some(true)
                    {
                        *event.input = call_event.input;
                        return Ok(Some(res));
                    }
                }
            }
            let res = runner
                .emit_tool_call(&mut call_event)
                .await
                .map_err(|e| e.to_string())?;
            *event.input = call_event.input;
            Ok(res)
        })
    }

    fn after_tool_call<'a>(
        &'a self,
        event: AfterToolCallArgs<'a>,
    ) -> BoxFuture<'a, Result<Option<ToolResultEventResult>, String>> {
        Box::pin(async move {
            let mut runner = self.runner.lock().await;
            if !runner.has_handlers(EventKind::ToolResult) {
                return Ok(None);
            }
            let content: Vec<ToolContent> = serde_json::from_value(
                serde_json::to_value(&event.result.content).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let usage: Option<Usage> = event
                .result
                .usage
                .as_ref()
                .map(|u| serde_json::to_value(u).and_then(serde_json::from_value))
                .transpose()
                .map_err(|e| e.to_string())?;
            let result_event = ToolResultEvent {
                tool_call_id: event.tool_call_id.to_owned(),
                tool_name: event.tool_name.to_owned(),
                input: event.input.clone(),
                content,
                details: Some(event.result.details.clone()),
                is_error: event.is_error,
                usage,
            };
            runner
                .emit_tool_result(result_event)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// A loopless runtime's own extensions' `tool_call`/`tool_result` handlers, emitted as a session
/// emits them.
pub fn extension_tool_handlers(
    runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    two_sided_home: TwoSidedHome,
) -> Arc<dyn LooplessToolHandlers> {
    Arc::new(RunnerToolHandlers {
        runner,
        two_sided_home,
        two_sided_paths: HashSet::new(),
    })
}

/// Variant with explicit two-sided extension paths when forwarding to the loop (S29).
pub fn extension_tool_handlers_with_two_sided(
    runner: Arc<Mutex<dyn LooplessExtensionRunner>>,
    two_sided_home: TwoSidedHome,
    two_sided_paths: HashSet<String>,
) -> Arc<dyn LooplessToolHandlers> {
    Arc::new(RunnerToolHandlers {
        runner,
        two_sided_home,
        two_sided_paths,
    })
}

#[cfg(test)]
#[async_trait]
impl LooplessExtensionRunner for maho_ext_host::runner::ExtensionRunner {
    fn has_handlers(&self, kind: EventKind) -> bool {
        maho_ext_host::runner::ExtensionRunner::has_handlers(self, kind)
    }

    async fn emit_tool_call(
        &mut self,
        event: &mut ToolCallEvent,
    ) -> Result<Option<ToolCallEventResult>, ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit_tool_call(self, event).await
    }

    async fn emit_tool_result(
        &mut self,
        event: ToolResultEvent,
    ) -> Result<Option<ToolResultEventResult>, ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit_tool_result(self, event).await
    }

    async fn emit_input(
        &mut self,
        input: InputEvent,
    ) -> Result<InputEventResult, ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit_input(self, input).await
    }

    async fn emit_before_agent_start(
        &mut self,
        event: BeforeAgentStartEvent,
    ) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit_before_agent_start(self, event).await
    }

    async fn emit_before_provider_request_with_metadata(
        &mut self,
        payload: Value,
        model: Option<Model>,
        headers: Option<BTreeMap<String, Option<String>>>,
        exclude_path: Option<&str>,
    ) -> Result<Value, ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit_before_provider_request_with_metadata(
            self,
            payload,
            model,
            headers,
            exclude_path,
        )
        .await
    }

    async fn emit(&mut self, event: ExtensionEvent) -> Result<(), ExtensionFailure> {
        maho_ext_host::runner::ExtensionRunner::emit(self, event)
            .await
            .map(|_| ())
    }

    fn on_error(&mut self, listener: LooplessExtensionErrorListener) {
        maho_ext_host::runner::ExtensionRunner::on_error(self, listener);
    }

    fn invalidate(&self, message: &str) {
        maho_ext_host::runner::ExtensionRunner::invalidate(self, message);
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        maho_ext_host::runner::ExtensionRunner::get_all_registered_tools(self)
    }

    fn filesystem_policy(&self) -> Option<FilesystemPolicyChecker> {
        let mut policies = Vec::new();
        for ext in self.extensions.iter() {
            let list = self
                .runtime
                .live_filesystem_policies(&ext.identity.path)
                .unwrap_or_else(|| ext.filesystem_policies.clone());
            policies.extend(list);
        }
        maho_tools::filesystem_policy::compose_filesystem_policies(policies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::types::AgentToolResult;
    use maho_ext_api::{
        BuildSystemPromptOptions, ComponentFactory, CustomComponentFactory, CustomUiFactoryOptions,
        CustomUiOptions, ExtensionContext, ExtensionFuture, ExtensionMode, ExtensionRuntime,
        ExtensionUi, ExtensionUiDialogOptions, ExtensionUiFactories, ExtensionWidgetOptions,
        FooterComponentFactory, Model, ModelRegistry, NotificationType, SessionEntry,
        SessionManager, Theme, ToolSessionManager, TuiComponentFactory, UiFuture, WidgetContent,
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
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, serde_json::Value> {
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
        ) -> ExtensionFuture<'_, serde_json::Value> {
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
    async fn empty_runner_allows_tool_calls_without_blocking() {
        let runner = Arc::new(Mutex::new(test_runner()));
        let handlers = extension_tool_handlers(runner, TwoSidedHome::Loop);
        let mut input = serde_json::json!({"path": "a.txt"});
        let before = handlers
            .before_tool_call(BeforeToolCallArgs {
                tool_name: "read",
                tool_call_id: "tc-1",
                input: &mut input,
                nested: None,
            })
            .await
            .expect("before");
        assert!(before.is_none());

        let result = AgentToolResult::text("ok");
        let after = handlers
            .after_tool_call(AfterToolCallArgs {
                tool_name: "read",
                tool_call_id: "tc-1",
                input: &input,
                result: &result,
                is_error: false,
                nested: None,
            })
            .await
            .expect("after");
        assert!(after.is_none());
    }
}
