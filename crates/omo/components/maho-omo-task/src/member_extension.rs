//! Native entry of `team/member-extension/index.ts` (the `omo-member.js` bundle a team member
//! process loads through `--extension`). The lead launches member processes with
//! [`crate::engine::MEMBER_EXTENSION_ENTRY`]; this adapter binds senpi-task's
//! `register_member_extension` to the host extension API.
use std::{collections::HashMap, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{DeliverAs, EventKind, EventResult, Extension, ExtensionApi, ExtensionFailure, ExtensionRuntime, ToolContent, ToolDefinition, ToolError, ToolResult};
use senpi_task::{store::{PersistedTaskEvent, StateDirConfig, TaskRecordStore}, team::member_extension::{index::{self as member, CustomMessage, ExtensionEventHandler, MemberExtensionOptions, SendMessageOptions, TaskEventAppender}, self_poller::TeamMessageEvent, tools::{MemberTaskSendInput, MemberTaskSendTool}}};

/// `export default registerMemberExtension`.
pub struct TaskMemberExtension;

impl Extension for TaskMemberExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let env: HashMap<String, String> = std::env::vars().collect();
        if let Err(error) = register_task_member_extension(api, env) { std::panic::panic_any(error); }
    }
}

/// Registers the member runtime for `env`; a process without a member identity registers nothing.
pub fn register_task_member_extension(api: &mut ExtensionApi, env: HashMap<String, String>) -> Result<(), ExtensionFailure> {
    let host = Arc::new(MemberHost { runtime: api.runtime.clone(), tools: Mutex::default(), handlers: Mutex::default() });
    let pi: Arc<dyn member::ExtensionApi> = host.clone();
    member::register_member_extension(&pi, MemberExtensionOptions {
        env,
        create_event_appender: Box::new(|parsed| {
            let store = TaskRecordStore::new(&StateDirConfig { project_dir: parsed.state_dir.clone().into(), task_state_dir: Some(parsed.state_dir.clone().into()) });
            let appender: TaskEventAppender = Arc::new(move |task_id: &str, event: TeamMessageEvent| {
                if let Err(error) = store.append_event(task_id, &PersistedTaskEvent { event_type: event.event_type, payload: event.payload }) {
                    maho_ext_api::diagnostic(&format!("senpi-task member event append failed: {error}"));
                }
            });
            appender
        }),
        scheduler: None,
    }).map_err(|error| ExtensionFailure::new(error.to_string()))?;
    for tool in std::mem::take(&mut *host.tools.lock().unwrap_or_else(PoisonError::into_inner)) { api.register_tool(member_tool_definition(tool)); }
    let events: Vec<EventKind> = host.handlers.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|(event, _)| *event).collect();
    for (index, event) in events.into_iter().enumerate() {
        let host = host.clone();
        api.on(event, Arc::new(move |_, _| {
            let result = host.dispatch(index);
            Box::pin(async move { result })
        }));
    }
    Ok(())
}

/// senpi `ExtensionAPI` slice the member extension drives, collected during registration and
/// committed onto the host API once registration returns.
struct MemberHost {
    runtime: ExtensionRuntime,
    tools: Mutex<Vec<MemberTaskSendTool>>,
    handlers: Mutex<Vec<(EventKind, ExtensionEventHandler)>>,
}

impl MemberHost {
    fn dispatch(&self, index: usize) -> Result<EventResult, ExtensionFailure> {
        let handlers = self.handlers.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((_, handler)) = handlers.get(index) else { return Ok(EventResult::None) };
        handler().map(|()| EventResult::None).map_err(|error| ExtensionFailure::new(error.to_string()))
    }
}

impl member::ExtensionApi for MemberHost {
    fn send_message(&self, message: CustomMessage, options: SendMessageOptions) {
        let deliver_as = match options.deliver_as.as_str() { "followUp" => DeliverAs::FollowUp, "nextTurn" => DeliverAs::NextTurn, _ => DeliverAs::Steer };
        let sent = self.runtime.message_actions().and_then(|actions| actions.send_message(
            maho_ext_api::CustomMessage { custom_type: message.custom_type, content: vec![ToolContent::text(message.content)], display: message.display, details: None },
            maho_ext_api::SendMessageOptions { trigger_turn: options.trigger_turn, deliver_as: Some(deliver_as) },
        ));
        if let Err(error) = sent { maho_ext_api::diagnostic(&format!("senpi-task member message injection failed: {}", error.message)); }
    }
    fn register_tool(&self, tool: MemberTaskSendTool) { self.tools.lock().unwrap_or_else(PoisonError::into_inner).push(tool); }
    fn on(&self, event: &str, handler: ExtensionEventHandler) {
        let kind = match event {
            "session_start" => EventKind::SessionStart,
            "session_shutdown" => EventKind::SessionShutdown,
            other => { maho_ext_api::diagnostic(&format!("senpi-task member extension: unsupported event {other}")); return; }
        };
        self.handlers.lock().unwrap_or_else(PoisonError::into_inner).push((kind, handler));
    }
}

/// `createMemberTaskSendTool`: the member-scoped `task_send` (no custom renderers upstream).
fn member_tool_definition(tool: MemberTaskSendTool) -> ToolDefinition {
    let tool = Arc::new(tool);
    let executor = tool.clone();
    let mut definition = ToolDefinition::new(tool.name, tool.description, tool.parameters.clone(), Arc::new(move |call| {
        let tool = executor.clone();
        Box::pin(async move {
            let params: MemberTaskSendInput = serde_json::from_value(call.params)?;
            let result = tool.execute(call.id, &params).map_err(|error| ToolError::Message(error.to_string()))?;
            let result: ToolResult = serde_json::from_value(serde_json::to_value(result)?)?;
            Ok(result)
        })
    }));
    definition.label = tool.label.into();
    definition
}
