//! `supervisor_work_tools`: typed supervisor work requests from a conversation session to its hub.
//!
//! The tools register only for a session opened with `context.supervisor_tools = "work"`. Each call
//! validates strictly typed arguments, emits one `supervisor.work_request` extension RPC event and
//! waits for the hub's `supervisor.work_reply` extension request carrying the same request id; the
//! wait ends on that reply or on the call's abort signal, never on a timer.
//!
//! The hub owns authority: actor, project, channel binding, source message and action grant are
//! derived there, never taken from model arguments. No field is a URL, command, path or free-form
//! JSON payload, so these tools cannot reach arbitrary HTTP or command execution, and owner actions
//! (approve, pause, resume, cancel, teach, forget) are not model-callable.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    ExtensionApi, ExtensionFailure, ExtensionSessionProfile, ToolDefinition, ToolError,
    ToolExecutionMode, ToolResult,
};
use maho_rpc::rpc_input_validation::{SUPERVISOR_TOOLS_CONTEXT_KEY, SUPERVISOR_TOOLS_WORK};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SUPERVISOR_WORK_TOOLS_PATH: &str = "<builtin:supervisor-work-tools>";
pub const SUPERVISOR_SUBMIT_WORK_TOOL: &str = "supervisor_submit_work";
pub const SUPERVISOR_WORK_STATUS_TOOL: &str = "supervisor_work_status";
pub const SUPERVISOR_WORK_TOOL_NAMES: [&str; 2] =
    [SUPERVISOR_SUBMIT_WORK_TOOL, SUPERVISOR_WORK_STATUS_TOOL];
/// Extension RPC event carrying one typed request to the hub.
pub const SUPERVISOR_WORK_REQUEST_EVENT: &str = "supervisor.work_request";
/// Extension request name the hub answers a work request with.
pub const SUPERVISOR_WORK_REPLY_REQUEST: &str = "supervisor.work_reply";
const MAX_TITLE_CHARS: usize = 200;
const MAX_REQUIREMENTS: usize = 20;
const MAX_REQUIREMENT_CHARS: usize = 4_000;
const MAX_GOAL_ID_CHARS: usize = 128;

type Pending = Arc<Mutex<BTreeMap<String, tokio::sync::oneshot::Sender<Value>>>>;

/// The session asked for the supervisor work tools.
pub fn supervisor_work_tools_requested(profile: &ExtensionSessionProfile) -> bool {
    profile
        .session_context
        .get(SUPERVISOR_TOOLS_CONTEXT_KEY)
        .map(String::as_str)
        == Some(SUPERVISOR_TOOLS_WORK)
}

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WorkerKind {
    Mhc,
    Pty,
    Any,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitWorkParams {
    title: String,
    requirements: Vec<String>,
    #[serde(default)]
    worker: Option<WorkerKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkStatusParams {
    #[serde(default)]
    goal_id: Option<String>,
}

fn bounded(field: &str, value: &str, max: usize) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if trimmed.chars().count() > max {
        return Err(format!("{field} exceeds {max} characters"));
    }
    Ok(trimmed.to_owned())
}

fn submit_payload(params: Value) -> Result<Value, String> {
    let params: SubmitWorkParams =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    if params.requirements.is_empty() || params.requirements.len() > MAX_REQUIREMENTS {
        return Err(format!(
            "requirements must list 1 to {MAX_REQUIREMENTS} items"
        ));
    }
    let requirements = params
        .requirements
        .iter()
        .map(|item| bounded("requirement", item, MAX_REQUIREMENT_CHARS))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"title":bounded("title", &params.title, MAX_TITLE_CHARS)?,"requirements":requirements,"worker":params.worker.unwrap_or(WorkerKind::Any)}),
    )
}

fn status_payload(params: Value) -> Result<Value, String> {
    let params: WorkStatusParams =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    let goal_id = params
        .goal_id
        .map(|goal| {
            let goal = bounded("goal_id", &goal, MAX_GOAL_ID_CHARS)?;
            if goal
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            {
                Ok(goal)
            } else {
                Err("goal_id must be an identifier".to_owned())
            }
        })
        .transpose()?;
    Ok(json!({"goalId":goal_id}))
}

/// Hands a hub reply to the waiting call; an unknown or malformed reply is a typed failure.
fn deliver_reply(pending: &Pending, data: Value) -> Result<Value, ExtensionFailure> {
    let failure = |message: String| {
        ExtensionFailure::classified(
            message,
            None,
            Some("supervisor_work_reply_invalid".to_owned()),
        )
    };
    let request_id = data
        .get("requestId")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("supervisor.work_reply requires a string requestId".into()))?;
    if !data.get("ok").is_some_and(Value::is_boolean) {
        return Err(failure(
            "supervisor.work_reply requires a boolean ok".into(),
        ));
    }
    let sender = pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(request_id)
        .ok_or_else(|| failure(format!("no pending supervisor work request {request_id}")))?;
    sender
        .send(data)
        .map_err(|_| failure("the supervisor work call already ended".into()))?;
    Ok(json!({"delivered":true}))
}

fn reply_result(reply: Value) -> Result<ToolResult, ToolError> {
    if reply["ok"] == true {
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        return Ok(ToolResult {
            content: vec![maho_ext_api::ToolContent::text(result.to_string())],
            details: Some(result),
            structured_content: None,
            is_error: None,
        });
    }
    let code = reply["error"]["code"].as_str().unwrap_or("rejected");
    let message = reply["error"]["message"]
        .as_str()
        .unwrap_or("the hub rejected the request");
    Err(ToolError::Message(format!(
        "supervisor_work_rejected: {code}: {message}"
    )))
}

struct WorkTool {
    name: &'static str,
    action: &'static str,
    description: &'static str,
    parameters: Value,
    payload: fn(Value) -> Result<Value, String>,
}

fn work_tool(
    tool: WorkTool,
    emitter: Arc<ExtensionApi>,
    pending: Pending,
    counter: Arc<AtomicU64>,
) -> ToolDefinition {
    let WorkTool {
        name,
        action,
        description,
        parameters,
        payload,
    } = tool;
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(move |call| {
            let (emitter, pending, counter) = (emitter.clone(), pending.clone(), counter.clone());
            Box::pin(async move {
                let payload = payload(call.params).map_err(|error| {
                    ToolError::Message(format!("supervisor_work_invalid: {error}"))
                })?;
                call.signal.check()?;
                let session = call
                    .context
                    .map(|context| context.session_manager().session_id().to_owned())
                    .unwrap_or_default();
                let request_id = format!(
                    "{session}:{action}:{}",
                    counter.fetch_add(1, Ordering::SeqCst) + 1
                );
                let (sender, receiver) = tokio::sync::oneshot::channel();
                pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(request_id.clone(), sender);
                let request = json!({"requestId":request_id,"sessionId":session,"action":action,"payload":payload});
                if let Err(error) = emitter.rpc_emit(SUPERVISOR_WORK_REQUEST_EVENT, &request) {
                    pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&request_id);
                    return Err(ToolError::Message(format!(
                        "supervisor_work_unavailable: {}",
                        error.message
                    )));
                }
                let reply = tokio::select! {
                    reply = receiver => reply.map_err(|_| ToolError::Message("supervisor_work_unavailable: the hub reply channel closed".into()))?,
                    () = call.signal.cancelled() => {
                        pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&request_id);
                        return Err(ToolError::Aborted);
                    }
                };
                reply_result(reply)
            })
        }),
    );
    definition.execution_mode = Some(ToolExecutionMode::Sequential);
    definition
}

/// Registers the typed supervisor work tools and the hub reply handler for a requesting session;
/// every other session gets nothing.
pub fn register_supervisor_work_tools(api: &mut ExtensionApi) -> Result<(), ExtensionFailure> {
    if !supervisor_work_tools_requested(&api.profile) {
        return Ok(());
    }
    let pending: Pending = Arc::default();
    let replies = pending.clone();
    api.rpc_handle(
        SUPERVISOR_WORK_REPLY_REQUEST,
        Arc::new(move |data| {
            let result = deliver_reply(&replies, data);
            Box::pin(async move { result })
        }),
    )?;
    let emitter = Arc::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    let counter = Arc::new(AtomicU64::new(0));
    let submit = WorkTool {
        name: SUPERVISOR_SUBMIT_WORK_TOOL,
        action: "submit",
        description: "Submit an execution request to the supervisor hub. The hub derives the requester, project, source message and action grant, authorizes the work and dispatches it; this tool only describes the work. Returns the hub's persisted admission receipt.",
        parameters: json!({"type":"object","additionalProperties":false,"required":["title","requirements"],"properties":{
            "title":{"type":"string","maxLength":MAX_TITLE_CHARS,"description":"Short title of the requested work."},
            "requirements":{"type":"array","minItems":1,"maxItems":MAX_REQUIREMENTS,"items":{"type":"string","maxLength":MAX_REQUIREMENT_CHARS},"description":"Verifiable requirements the work must satisfy."},
            "worker":{"type":"string","enum":["mhc","pty","any"],"description":"Preferred worker kind; the hub decides."}}}),
        payload: submit_payload,
    };
    let status = WorkTool {
        name: SUPERVISOR_WORK_STATUS_TOOL,
        action: "status",
        description: "Read the supervisor hub's current state of admitted work in this conversation's scope, or of one goal.",
        parameters: json!({"type":"object","additionalProperties":false,"properties":{
            "goal_id":{"type":"string","maxLength":MAX_GOAL_ID_CHARS,"description":"Goal id from an earlier receipt; omit for the whole scope."}}}),
        payload: status_payload,
    };
    for tool in [submit, status] {
        api.try_register_tool(work_tool(
            tool,
            emitter.clone(),
            pending.clone(),
            counter.clone(),
        ))?;
    }
    Ok(())
}

/// The always-assembled builtin factory; it registers nothing unless the session asks for it.
pub fn supervisor_work_tools_factory() -> maho_ext_host::loader::NativeAsyncExtensionFactory {
    maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: SUPERVISOR_WORK_TOOLS_PATH.to_owned(),
        source_info: super::default_extensions::builtin_source_info(SUPERVISOR_WORK_TOOLS_PATH),
        factory: Arc::new(|api| {
            let result = register_supervisor_work_tools(api);
            Box::pin(async move { result })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(context: &[(&str, &str)]) -> ExtensionApi {
        let profile = ExtensionSessionProfile {
            session_context: context
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            ..Default::default()
        };
        ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                SUPERVISOR_WORK_TOOLS_PATH,
                Default::default(),
                Default::default(),
            ),
            profile,
            Default::default(),
            Default::default(),
        )
    }

    #[test]
    fn only_the_work_role_registers_tools() {
        let mut plain = api(&[]);
        register_supervisor_work_tools(&mut plain).unwrap();
        assert!(plain.registered.tools.is_empty() && plain.registered.rpc_handlers.is_empty());
        let mut work = api(&[("supervisor_tools", "work")]);
        register_supervisor_work_tools(&mut work).unwrap();
        let names = work
            .registered
            .tools
            .iter()
            .map(|tool| tool.definition.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, SUPERVISOR_WORK_TOOL_NAMES);
        assert!(
            work.registered
                .rpc_handlers
                .contains_key(SUPERVISOR_WORK_REPLY_REQUEST)
        );
    }

    #[test]
    fn arguments_are_typed_and_closed() {
        assert!(submit_payload(json!({"title":"t","requirements":["r"],"worker":"pty"})).is_ok());
        for invalid in [
            json!({"title":"t","requirements":[]}),
            json!({"title":" ","requirements":["r"]}),
            json!({"title":"t","requirements":["r"],"url":"http://x"}),
            json!({"title":"t","requirements":["r"],"command":"rm -rf /"}),
            json!({"title":"t","requirements":["r"],"worker":"shell"}),
            json!({"title":"t","requirements":[""]}),
        ] {
            assert!(submit_payload(invalid.clone()).is_err(), "{invalid}");
        }
        assert_eq!(status_payload(json!({})).unwrap(), json!({"goalId":null}));
        assert!(status_payload(json!({"goal_id":"../etc"})).is_err());
        assert!(status_payload(json!({"goal_id":"g-1","action":"approve"})).is_err());
    }

    #[tokio::test]
    async fn a_call_round_trips_through_the_hub_reply() {
        let mut work = api(&[("supervisor_tools", "work")]);
        let (seen, request) = tokio::sync::oneshot::channel::<Value>();
        let seen = Mutex::new(Some(seen));
        let _subscription = work.events.on(
            "senpi:extension-rpc-event",
            Arc::new(move |event| {
                if event["name"] == SUPERVISOR_WORK_REQUEST_EVENT
                    && let Some(seen) = seen.lock().unwrap_or_else(PoisonError::into_inner).take()
                {
                    let _ = seen.send(event["data"].clone());
                }
            }),
        );
        register_supervisor_work_tools(&mut work).unwrap();
        let submit = work
            .registered
            .tools
            .iter()
            .find(|tool| tool.definition.name == SUPERVISOR_SUBMIT_WORK_TOOL)
            .unwrap()
            .definition
            .execute
            .clone();
        let reply = work.registered.rpc_handlers[SUPERVISOR_WORK_REPLY_REQUEST].clone();
        let call = tokio::spawn(async move {
            submit(maho_ext_api::ToolCall {
                id: "call",
                params: json!({"title":"ship","requirements":["tests pass"]}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await
        });
        let request = request.await.unwrap();
        assert_eq!(request["action"], "submit");
        assert_eq!(
            request["payload"],
            json!({"title":"ship","requirements":["tests pass"],"worker":"any"})
        );
        assert!(
            reply(json!({"requestId":"unknown","ok":true}))
                .await
                .is_err()
        );
        reply(json!({"requestId":request["requestId"],"ok":true,"result":{"goalId":"g-1","state":"ready"}})).await.unwrap();
        let result = call.await.unwrap().unwrap();
        assert_eq!(
            result.details,
            Some(json!({"goalId":"g-1","state":"ready"}))
        );
    }

    #[tokio::test]
    async fn an_aborted_call_stops_waiting_and_forgets_its_request() {
        let mut work = api(&[("supervisor_tools", "work")]);
        let (seen, request) = tokio::sync::oneshot::channel::<Value>();
        let seen = Mutex::new(Some(seen));
        let _subscription = work.events.on(
            "senpi:extension-rpc-event",
            Arc::new(move |event| {
                if let Some(seen) = seen.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    let _ = seen.send(event["data"].clone());
                }
            }),
        );
        register_supervisor_work_tools(&mut work).unwrap();
        let status = work
            .registered
            .tools
            .iter()
            .find(|tool| tool.definition.name == SUPERVISOR_WORK_STATUS_TOOL)
            .unwrap()
            .definition
            .execute
            .clone();
        let reply = work.registered.rpc_handlers[SUPERVISOR_WORK_REPLY_REQUEST].clone();
        let signal = maho_ext_api::AbortSignal::default();
        let call_signal = signal.clone();
        let call = tokio::spawn(async move {
            status(maho_ext_api::ToolCall {
                id: "call",
                params: json!({}),
                signal: call_signal,
                on_update: None,
                context: None,
            })
            .await
        });
        let request = request.await.unwrap();
        signal.abort();
        assert!(matches!(call.await.unwrap(), Err(ToolError::Aborted)));
        assert!(
            reply(json!({"requestId":request["requestId"],"ok":true}))
                .await
                .is_err()
        );
    }
}
