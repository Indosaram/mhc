//! Unified `thread_create` body for new/existing/forked local threads (`thread/control/create.ts`).

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::super::address::{ResolveContext, ResolveOptions, resolve_address};
use super::super::address_book::to_thread_address_entries;
use super::super::addressing::{ThreadNameConflictResult, check_name_conflict};
use super::super::contracts::params::ThreadCreateInput;
use super::super::errors::thread_tool_failure;
use super::super::initial_skills::{InitialSkillsCheck, check_initial_skills};
use super::super::thread_link::thread_link;
use super::super::tools::internals::{
    address_book, failure, send_address_book_async, send_book_entries_async, summary,
    target_session, uncertain_failure,
};
use super::super::tools::ports::{BoxFuture, ThreadHostView, ThreadToolSurfaceOptions};
use super::super::tools::thread_row::opened_result;
use super::create_launch::{CreateLaunchPlan, create_launch};
use super::local_session_host::{LocalSessionHost, default_worktree_root, workspace_note};
use super::locate::{LocatedControl, locate_control};
use super::prompt_start::PromptStart;
use super::session_host::{
    HostedThread, SessionHost, SessionOpenOutcome, SessionOpenRequest, created_thread_id,
};

#[derive(Debug, Clone)]
pub struct CreatePromptSend {
    pub thread: String,
    pub message: String,
    pub delivery: Option<String>,
}

pub type DeliverPromptFn = Arc<
    dyn Fn(ThreadHostView, String, CreatePromptSend, String) -> BoxFuture<Result<Value, String>>
        + Send
        + Sync,
>;

pub struct CreateContext {
    pub operation_id: String,
    pub caller_id: String,
    pub retrying: Option<Option<String>>,
    pub view: Arc<dyn Fn() -> BoxFuture<Result<ThreadHostView, String>> + Send + Sync>,
    pub deliver_prompt: DeliverPromptFn,
    pub await_prompt_start: Arc<dyn Fn(String) -> BoxFuture<PromptStart> + Send + Sync>,
    pub record_creator: Arc<dyn Fn(String, Value) -> BoxFuture<()> + Send + Sync>,
}

#[allow(clippy::large_enum_variant)]
enum LocalExisting {
    Live(HostedThread),
    File {
        session_path: String,
        name: Option<String>,
        cwd: Option<String>,
    },
    Error(Value),
}

async fn existing_here(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    address: &str,
    caller_id: &str,
) -> LocalExisting {
    let entries = send_book_entries_async(options, current, address).await;
    let ctx = ResolveContext {
        remote: None,
        caller_workspace_root: (options.caller_workspace_root)(),
        local: entries,
        local_for: None,
        caller_id: caller_id.to_owned(),
    };
    let resolved = resolve_address(
        &ctx,
        address,
        &ResolveOptions {
            all_scope: true,
            ..Default::default()
        },
    )
    .await;
    let thread = match resolved {
        Err(err) => return LocalExisting::Error(json!({ "kind": "error", "error": err })),
        Ok(r) => r.thread,
    };
    if let Some(live) = target_session(current, &thread) {
        return LocalExisting::Live(HostedThread {
            summary: summary(live, None),
            worktree: None,
            warnings: None,
        });
    }
    let book = send_address_book_async(options, current, &thread, true).await;
    let entry = book.into_iter().find(|c| c.thread_id == thread);
    let Some(entry) = entry else {
        return LocalExisting::Error(failure(
            "not_resumable",
            "The thread has no session file on this device to open.",
            "Call thread_list (tool.thread_list() in an eval cell) and pick a thread it lists.",
            None,
        ));
    };
    let Some(session_path) = entry.session_path else {
        return LocalExisting::Error(failure(
            "not_resumable",
            "The thread has no session file on this device to open.",
            "Call thread_list (tool.thread_list() in an eval cell) and pick a thread it lists.",
            None,
        ));
    };
    LocalExisting::File {
        session_path,
        name: entry.name.filter(|n| !n.trim().is_empty()),
        cwd: Path::new(&entry.cwd).is_absolute().then_some(entry.cwd),
    }
}

pub async fn create_thread(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    value: &ThreadCreateInput,
    context: CreateContext,
) -> Result<Value, String> {
    if let Some(device) = value.device.as_deref() {
        return Ok(failure(
            "remote_unavailable",
            format!("Device {device} is unavailable without remote relay (O02)."),
            "Drop device to create a thread on this device.",
            Some(json!({ "device": device })),
        ));
    }

    let creator_preset = options.permission_preset.as_ref().and_then(|f| f());
    let (permission_preset, profile) = match create_launch(value, creator_preset.as_deref()) {
        CreateLaunchPlan::Error(err) => return Ok(err),
        CreateLaunchPlan::Ok {
            permission_preset,
            profile,
        } => (permission_preset, profile),
    };

    if let Some(prompt) = value.prompt.as_deref()
        && prompt.trim().is_empty()
    {
        return Ok(failure(
            "invalid_arguments",
            "prompt is blank, so it is no first message; nothing was created.",
            "Pass the new thread's first message as prompt, or drop prompt and pass name to open an empty named thread.",
            Some(json!({ "field": "prompt" })),
        ));
    }

    let named = value.name.as_deref().is_some_and(|n| !n.trim().is_empty())
        || value.alias.as_deref().is_some_and(|a| !a.trim().is_empty());
    if value.thread.is_none() && value.fork_from.is_none() && value.prompt.is_none() && !named {
        return Ok(failure(
            "invalid_arguments",
            "thread_create (tool.thread_create() in an eval cell) makes a new thread only with a prompt or a name; neither was given, so nothing was created.",
            "Pass prompt (the new thread's first message) or name, or pass thread to open an existing thread.",
            None,
        ));
    }

    if let Some(note) = &context.retrying {
        let derived_id = created_thread_id(&context.caller_id, &context.operation_id);
        match existing_here(options, current, &derived_id, &context.caller_id).await {
            LocalExisting::Live(_) | LocalExisting::File { .. } => {}
            LocalExisting::Error(_) => return Ok(uncertain_failure(note.as_deref())),
        }
    }

    let loaded_skills = match value.skills.as_deref() {
        Some(skills) if !skills.is_empty() => match check_initial_skills(skills) {
            InitialSkillsCheck::Ok { names } => Some(names),
            InitialSkillsCheck::Invalid { names } => {
                return Ok(failure(
                    "invalid_arguments",
                    format!(
                        "Not a skill name: {}. Pass the names of installed skills, never a path or text.",
                        names.join(", ")
                    ),
                    "Pass skill names as listed by the skill index.",
                    Some(json!({ "invalid": names })),
                ));
            }
        },
        _ => None,
    };

    let caller_cwd = (options.caller_workspace_root)();
    let entries = to_thread_address_entries(&address_book(options, current, &[]));
    let visible_summaries: Vec<super::super::contracts::results::ThreadSummary> =
        entries.into_iter().map(|e| e.summary).collect();
    let local_host = LocalSessionHost {
        host: options.host.clone(),
        visible: visible_summaries.clone(),
        worktree_root: options
            .worktree_root
            .clone()
            .unwrap_or_else(|| Arc::new(default_worktree_root)),
    };

    let mut creates = false;
    let mut reopened_id: Option<String> = None;
    let mut previous_name: Option<String> = None;
    let mut open_req = SessionOpenRequest {
        cwd: value
            .cwd
            .clone()
            .or_else(|| (!caller_cwd.trim().is_empty()).then_some(caller_cwd.clone())),
        worktree: value.worktree.clone(),
        ..Default::default()
    };

    if let Some(existing_addr) = value.thread.as_deref() {
        match existing_here(options, current, existing_addr, &context.caller_id).await {
            LocalExisting::Error(err) => return Ok(err),
            LocalExisting::Live(thread) => {
                let made = opened_result(
                    &thread.summary,
                    thread.worktree.as_ref(),
                    thread.warnings.as_deref(),
                    true,
                );
                return finish_create(options, current, value, &context, made, Some(thread)).await;
            }
            LocalExisting::File {
                session_path,
                name,
                cwd,
            } => {
                reopened_id = Some(existing_addr.to_owned());
                previous_name = name;
                open_req = SessionOpenRequest {
                    session_path: Some(session_path),
                    cwd,
                    ..Default::default()
                };
            }
        }
    } else if let Some(fork_addr) = value.fork_from.as_deref() {
        match existing_here(options, current, fork_addr, &context.caller_id).await {
            LocalExisting::Error(err) => return Ok(err),
            LocalExisting::Live(thread) => {
                let Some(path) = thread.summary.session_path else {
                    return Ok(failure(
                        "not_resumable",
                        "The thread has no session file on this device to fork.",
                        "Pick a thread with a persisted session file.",
                        None,
                    ));
                };
                open_req.fork_from = Some(path);
                creates = true;
            }
            LocalExisting::File { session_path, .. } => {
                open_req.fork_from = Some(session_path);
                creates = true;
            }
        }
    } else {
        creates = true;
    }

    let local_alias = value.alias.is_some();
    let effective_name = if local_alias {
        value
            .name
            .as_deref()
            .or_else(|| value.alias.as_deref().map(str::trim))
            .map(str::to_owned)
    } else {
        value.name.clone()
    };

    let created_id = creates.then(|| created_thread_id(&context.caller_id, &context.operation_id));
    let earlier = match created_id.as_deref() {
        Some(id) => match existing_here(options, current, id, &context.caller_id).await {
            LocalExisting::Live(t) => Some(LocalExisting::Live(t)),
            LocalExisting::File {
                session_path,
                name,
                cwd,
            } => Some(LocalExisting::File {
                session_path,
                name,
                cwd,
            }),
            LocalExisting::Error(_) => None,
        },
        None => None,
    };

    if creates
        && let Some(ref name) = effective_name
        && earlier.is_none()
    {
        let filtered: Vec<_> = visible_summaries
            .iter()
            .filter(|t| reopened_id.as_deref() != Some(t.thread_id.as_str()))
            .cloned()
            .collect();
        if let ThreadNameConflictResult::Error(err) = check_name_conflict(&filtered, name) {
            return Ok(json!({ "kind": "error", "error": err }));
        }
    }

    let kept_name = if creates {
        None
    } else {
        effective_name.clone().or(previous_name)
    };
    if creates {
        open_req.name = effective_name.clone();
        open_req.initial_skills = loaded_skills.clone();
        open_req.permission_preset = permission_preset;
        open_req.provider = profile.provider;
        open_req.model_id = profile.model_id;
        open_req.thinking_level = profile.thinking_level;
        open_req.durable_session_id = created_id.clone();
    }

    let outcome = match earlier {
        Some(LocalExisting::Live(t)) => SessionOpenOutcome::Ok(t),
        Some(LocalExisting::File {
            session_path, cwd, ..
        }) => {
            local_host
                .open(
                    SessionOpenRequest {
                        session_path: Some(session_path),
                        cwd,
                        ..Default::default()
                    },
                    creates,
                )
                .await?
        }
        _ => match local_host.open(open_req, creates).await {
            Ok(out) => out,
            Err(err) => {
                if let Some(ref cid) = created_id
                    && let Ok(fresh_view) = (context.view)().await
                {
                    match existing_here(options, &fresh_view, cid, &context.caller_id).await {
                        LocalExisting::Live(t) => SessionOpenOutcome::Ok(t),
                        LocalExisting::File {
                            session_path, cwd, ..
                        } => {
                            local_host
                                .open(
                                    SessionOpenRequest {
                                        session_path: Some(session_path),
                                        cwd,
                                        ..Default::default()
                                    },
                                    false,
                                )
                                .await?
                        }
                        LocalExisting::Error(_) => return Err(err),
                    }
                } else {
                    return Err(err);
                }
            }
        },
    };

    let thread = match outcome {
        SessionOpenOutcome::Error(err) => return Ok(err),
        SessionOpenOutcome::Ok(t) => t,
    };

    let note_thread = thread.clone();
    let note_cwd = caller_cwd.clone();
    let note = tokio::task::spawn_blocking(move || workspace_note(&note_thread, &note_cwd))
        .await
        .expect("workspace note worker failed");
    let mut made = opened_result(
        &thread.summary,
        thread.worktree.as_ref(),
        thread.warnings.as_deref(),
        false,
    );
    if let Some(obj) = made.as_object_mut() {
        if creates && let Some(ref skills) = loaded_skills {
            obj.insert("requested_skills".into(), json!(skills));
        }
        if let Value::Object(note) = note {
            for (k, v) in note {
                obj.insert(k, v);
            }
        }
    }

    if creates {
        (context.record_creator)(context.caller_id.clone(), made.clone()).await;
    } else if let Some(ref wanted) = kept_name {
        let trimmed = wanted.trim();
        if !trimmed.is_empty()
            && let Ok(fresh_view) = (context.view)().await
            && let LocatedControl::Control(ctrl) = locate_control(
                options,
                &fresh_view,
                &thread.summary.thread_id,
                true,
                &context.caller_id,
            )
            .await
            && ctrl.set_name(trimmed).await.is_ok()
            && let Some(obj) = made.as_object_mut()
        {
            obj.insert("name".into(), Value::String(trimmed.to_owned()));
            obj.insert(
                "link".into(),
                Value::String(thread_link(&thread.summary.thread_id, Some(trimmed))),
            );
        }
    }

    if let Some(alias) = value.alias.as_deref()
        && let Some(obj) = made.as_object_mut()
    {
        obj.insert("alias_scope".into(), Value::String("local".into()));
        obj.insert(
            "alias_unavailable".into(),
            Value::String("remote_unavailable".into()),
        );
        let _ = alias;
    }

    finish_create(options, current, value, &context, made, Some(thread)).await
}

async fn finish_create(
    _options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    value: &ThreadCreateInput,
    context: &CreateContext,
    mut made: Value,
    opened: Option<HostedThread>,
) -> Result<Value, String> {
    if value.prompt.is_none() {
        if made.get("kind").and_then(Value::as_str) == Some("ok")
            && let Some(obj) = made.as_object_mut()
        {
            obj.insert("prompt".into(), Value::Null);
        }
        return Ok(made);
    }
    let Some(thread) = opened else {
        return Ok(made);
    };
    let send = CreatePromptSend {
        thread: thread.summary.thread_id.clone(),
        message: value.prompt.clone().unwrap_or_default(),
        delivery: value.delivery.clone(),
    };
    let sent = match (context.deliver_prompt)(
        current.clone(),
        thread.summary.thread_id.clone(),
        send,
        format!("{}:prompt", context.operation_id),
    )
    .await
    {
        Ok(v) => v,
        Err(err) => {
            if let Some(obj) = made.as_object_mut() {
                obj.insert(
                    "prompt".into(),
                    json!({
                        "error": thread_tool_failure(
                            "internal_error",
                            format!("The thread was created, but its first prompt was not delivered: {err}."),
                            format!("Send it with thread_send to {} (tool.thread_send({{ thread, message }}) in an eval cell); read the thread first if the prompt may have arrived.", thread.summary.thread_id),
                            Some(json!({ "thread": thread.summary.thread_id })),
                        )
                    }),
                );
            }
            return Ok(made);
        }
    };
    if sent.get("kind").and_then(Value::as_str) == Some("error") {
        if let Some(obj) = made.as_object_mut() {
            obj.insert("prompt".into(), json!({ "error": sent.get("error") }));
        }
        return Ok(made);
    }
    let mut prompt_obj = Map::new();
    let delivery_val = sent.get("delivery").cloned().unwrap_or(Value::Null);
    prompt_obj.insert("delivery".into(), delivery_val.clone());
    if let Some(v) = sent.get("deduplicated") {
        prompt_obj.insert("deduplicated".into(), v.clone());
    }
    let delivery_id = sent
        .get("delivery_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(ref did) = delivery_id {
        prompt_obj.insert("delivery_id".into(), Value::String(did.clone()));
    }
    if let Some(v) = sent.get("effective_mode") {
        prompt_obj.insert("effective_mode".into(), v.clone());
    }

    let delivery_kind = delivery_val.get("kind").and_then(Value::as_str);
    if matches!(delivery_kind, Some("started" | "steered")) {
        if let Some(obj) = made.as_object_mut() {
            obj.insert("prompt".into(), Value::Object(prompt_obj));
        }
        return Ok(made);
    }

    let waiting_kind = delivery_val
        .get("waiting_on")
        .and_then(|w| w.get("kind"))
        .and_then(Value::as_str);
    if delivery_kind == Some("queued") && waiting_kind == Some("wake") {
        prompt_obj.insert(
            "next_action".into(),
            Value::String(format!(
                "The prompt is stored and runs when {} wakes: do not send it again. Check it with thread_read (tool.thread_read({{ thread }}) in an eval cell).",
                thread.summary.thread_id
            )),
        );
        if let Some(obj) = made.as_object_mut() {
            obj.insert("prompt".into(), Value::Object(prompt_obj));
        }
        return Ok(made);
    }

    if let Some(did) = delivery_id {
        match (context.await_prompt_start)(did.clone()).await {
            PromptStart::Started { delivery } => {
                prompt_obj.insert(
                    "delivery".into(),
                    serde_json::to_value(delivery).unwrap_or(delivery_val),
                );
                if let Some(obj) = made.as_object_mut() {
                    obj.insert("prompt".into(), Value::Object(prompt_obj));
                }
                return Ok(made);
            }
            PromptStart::Queued { waiting_on } => {
                let pos = delivery_val
                    .get("queue_position")
                    .and_then(Value::as_i64)
                    .unwrap_or(1);
                let mut q = json!({ "kind": "queued", "queue_position": pos });
                if let Some(w) = waiting_on
                    && let Some(qo) = q.as_object_mut()
                {
                    qo.insert(
                        "waiting_on".into(),
                        serde_json::to_value(w).unwrap_or(Value::Null),
                    );
                }
                prompt_obj.insert("delivery".into(), q);
                if let Some(obj) = made.as_object_mut() {
                    obj.insert("prompt".into(), Value::Object(prompt_obj));
                }
                return Ok(made);
            }
            PromptStart::NotStarted { state } => {
                let tid = &thread.summary.thread_id;
                return Ok(failure(
                    "prompt_not_started",
                    format!(
                        "Thread {tid} was created, but its first prompt did not start a turn (its delivery is {state})."
                    ),
                    format!(
                        "The thread exists: read it with thread_read (tool.thread_read({{ thread }}) in an eval cell) to see whether the prompt ran, then send the prompt again with thread_send to {tid} (tool.thread_send({{ thread, message }}) in an eval cell) if it did not. Do not create the thread again."
                    ),
                    Some(json!({ "thread": tid, "delivery_id": did, "state": state })),
                ));
            }
        }
    }

    if let Some(obj) = made.as_object_mut() {
        obj.insert("prompt".into(), Value::Object(prompt_obj));
    }
    Ok(made)
}
