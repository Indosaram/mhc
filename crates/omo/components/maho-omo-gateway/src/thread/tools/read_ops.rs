//! `thread_list` and `thread_read` local operations (`thread/tools/read-ops.ts`).

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::super::address_book::to_thread_address_entries;
use super::super::addressing::{workspace_directories, workspace_entries};
use super::super::contracts::params::ThreadReadInput;
use super::super::contracts::results::ThreadWaitOutcome;
use super::super::contracts::state::ThreadState;
use super::super::control::locate::{LocatedControl, locate_control};
use super::super::control::port::{ControlError, SessionControl, control_identity};
use super::super::disk_list::{ListDiskSessionsOptions, list_disk_sessions};
use super::super::paged_reader::{EntryPageRead, is_entry_cursor, stale_entry_cursor};
use super::super::reader::{
    ThreadReadMode, ThreadReadOptions, ThreadReadSourceRef, ThreadTranscriptEntry,
    ThreadTranscriptReadResult, read_transcript,
};
use super::super::store_api::GatewayStoreApi;
use super::super::thread_link::thread_link;
use super::internals::{
    ThreadToolSummary, address_book, degraded_summary, failure, send_address_book_async, summary,
};
use super::ports::{ThreadHostSession, ThreadHostView, ThreadToolSurfaceOptions};
use super::read_item::read_item;
use super::thread_row::{row_named, thread_row};
use super::thread_state::listed_states;

pub fn list_threads(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    all_scope: Option<bool>,
    states: Option<&[ThreadState]>,
) -> Value {
    let all = all_scope.unwrap_or(false);
    let root = (options.caller_workspace_root)();
    let scanned = options
        .sessions_directory
        .as_ref()
        .map(|f| {
            list_disk_sessions(
                &f(),
                &ListDiskSessionsOptions {
                    all_scope: all,
                    workspace_roots: workspace_directories(&root),
                },
            )
        })
        .unwrap_or_default();
    let book = address_book(options, current, &scanned.sessions);
    let scoped = workspace_entries(&to_thread_address_entries(&book), Some(&root));
    let scoped_ids: HashSet<String> = scoped.into_iter().map(|e| e.summary.thread_id).collect();
    let in_scope = |thread_id: &str| all || scoped_ids.contains(thread_id);

    let visible: Vec<&ThreadHostSession> = current
        .sessions
        .iter()
        .filter(|s| in_scope(s.durable_session_id.as_deref().unwrap_or(&s.session_id)))
        .collect();
    let listed: HashSet<String> = visible
        .iter()
        .map(|s| {
            s.durable_session_id
                .as_deref()
                .unwrap_or(&s.session_id)
                .to_owned()
        })
        .collect();

    let scanned_ids: HashSet<&str> = scanned
        .sessions
        .iter()
        .map(|s| s.durable_id.as_str())
        .collect();

    let inactive: Vec<_> = book
        .iter()
        .filter(|entry| {
            entry.status != super::super::contracts_results::ThreadStatus::Live
                && (entry.error_note.is_some() || scanned_ids.contains(entry.durable_id.as_str()))
                && !listed.contains(&entry.thread_id)
                && in_scope(&entry.thread_id)
        })
        .collect();

    let order: HashMap<&str, usize> = book
        .iter()
        .enumerate()
        .map(|(i, e)| (e.thread_id.as_str(), i))
        .collect();
    let shown = listed_states(states);

    let mut combined: Vec<ThreadToolSummary> = visible
        .iter()
        .map(|session| {
            let sid = session
                .durable_session_id
                .as_deref()
                .unwrap_or(&session.session_id);
            let entry = book.iter().find(|e| {
                e.thread_id == sid
                    && (session.socket.as_deref().is_none()
                        || e.source_host.as_deref() == session.socket.as_deref())
            });
            row_named(
                summary(session, entry),
                entry
                    .and_then(|e| e.name.as_deref())
                    .or(session.name.as_deref()),
                entry.and_then(|e| e.title.as_deref()),
            )
        })
        .collect();

    for entry in inactive {
        combined.push(row_named(
            degraded_summary(entry),
            entry.name.as_deref(),
            entry.title.as_deref(),
        ));
    }

    combined.retain(|t| shown.contains(&t.state));
    combined.sort_by_key(|t| {
        order
            .get(t.thread_id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

    let threads: Vec<Value> = combined.iter().map(|t| thread_row(t, None)).collect();
    let row_ids: HashSet<String> = threads
        .iter()
        .filter_map(|t| t.get("thread").and_then(Value::as_str).map(str::to_owned))
        .collect();

    let dropped =
        if shown.contains(&ThreadState::Inactive) || shown.contains(&ThreadState::Archived) {
            scanned
                .unread
                .iter()
                .filter(|id| !listed.contains(*id) && !row_ids.contains(*id))
                .count()
        } else {
            0
        };

    let unanswered = current
        .hosts
        .iter()
        .filter(|host| {
            (host.error.is_some() || host.alive == Some(false))
                && host.reason.as_deref() != Some("dead")
        })
        .count();

    let mut out = json!({
        "kind": "ok",
        "threads": threads,
        "scope": if all { "all" } else { "workspace" },
    });
    if let Some(obj) = out.as_object_mut() {
        if dropped > 0 {
            obj.insert("inactive_truncated".into(), Value::Bool(true));
            obj.insert(
                "inactive_dropped".into(),
                Value::Number((dropped as u64).into()),
            );
        }
        if unanswered > 0 {
            obj.insert("listing".into(), Value::String("incomplete".into()));
            obj.insert(
                "unanswered_endpoints".into(),
                Value::Number((unanswered as u64).into()),
            );
        }
    }
    out
}

pub async fn list_threads_async(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    all_scope: Option<bool>,
    states: Option<&[ThreadState]>,
) -> Value {
    let options = options.clone();
    let current = current.clone();
    let states = states.map(<[ThreadState]>::to_vec);
    tokio::task::spawn_blocking(move || {
        list_threads(&options, &current, all_scope, states.as_deref())
    })
    .await
    .expect("thread list worker failed")
}

pub fn with_backlog(store: &dyn GatewayStoreApi, mut listed: Value) -> Value {
    if listed.get("kind").and_then(Value::as_str) != Some("ok") {
        return listed;
    }
    let Some(threads_arr) = listed.get("threads").and_then(Value::as_array) else {
        return listed;
    };
    if threads_arr.is_empty() {
        return listed;
    }
    let ids: Vec<String> = threads_arr
        .iter()
        .filter_map(|t| t.get("thread").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let backlogs = match store.backlogs(&ids) {
        Ok(b) => b,
        Err(err) => {
            if let Some(obj) = listed.as_object_mut() {
                obj.insert("backlog_error".into(), Value::String(err.message()));
            }
            return listed;
        }
    };
    if let Some(threads_mut) = listed.get_mut("threads").and_then(Value::as_array_mut) {
        for thread in threads_mut {
            let Some(tid) = thread
                .get("thread")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            if let Some(found) = backlogs.get(&tid)
                && let Some(oldest_at) = found.oldest_at
                && let Some(obj) = thread.as_object_mut()
            {
                let iso = chrono::DateTime::from_timestamp_millis(oldest_at)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| oldest_at.to_string());
                obj.insert(
                    "backlog".into(),
                    json!({
                        "held": found.held,
                        "held_by_session": found.held_by_session,
                        "waiting_in_gateway": found.waiting_in_gateway,
                        "oldest_at": iso,
                    }),
                );
            }
        }
    }
    listed
}

fn read_options(value: &ThreadReadInput) -> ThreadReadOptions {
    ThreadReadOptions {
        mode: Some(if value.cursor.is_none() {
            ThreadReadMode::Tail
        } else {
            ThreadReadMode::Page
        }),
        tail_items: None,
        max_bytes: value.max_bytes,
        cursor: value.cursor.clone(),
    }
}

pub async fn read_thread(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    value: &ThreadReadInput,
    caller_id: &str,
) -> Result<Value, ControlError> {
    let located = locate_control(
        options,
        current,
        &value.thread,
        value.all_scope.unwrap_or(false),
        caller_id,
    )
    .await;
    match located {
        LocatedControl::Error(err) => Ok(err),
        LocatedControl::NotLive { thread_id, .. } => {
            Ok(read_degraded(options, current, &thread_id, value).await)
        }
        LocatedControl::Control(ctrl) => read_live(ctrl.as_ref(), value).await,
    }
}

async fn read_live(
    control: &dyn SessionControl,
    value: &ThreadReadInput,
) -> Result<Value, ControlError> {
    let wait: Option<ThreadWaitOutcome> = match value.wait_ms {
        Some(ms) => match control.wait_settled(ms).await {
            Ok(w) => Some(w),
            Err(ControlError::Refusal(r)) => {
                return Ok(json!({ "kind": "error", "error": r.failure }));
            }
            Err(err) => return Err(err),
        },
        None => None,
    };

    let mut res = match paged_read_result(control, value).await? {
        Some(r) => r,
        None => {
            let messages = control.get_messages().await?;
            live_read_result(control.thread_id(), Some(control.title()), messages, value)
        }
    };

    if res.get("kind").and_then(Value::as_str) == Some("ok")
        && let Some(obj) = res.as_object_mut()
    {
        if let Value::Object(ident) = control_identity(control.thread_id(), control.device()) {
            for (k, v) in ident {
                obj.insert(k, v);
            }
        }
        if let Some(w) = wait {
            obj.insert(
                "wait".into(),
                serde_json::to_value(w).unwrap_or(Value::Null),
            );
        }
    }
    Ok(res)
}

async fn paged_read_result(
    control: &dyn SessionControl,
    value: &ThreadReadInput,
) -> Result<Option<Value>, ControlError> {
    let entry_cursor = value.cursor.as_deref().is_some_and(is_entry_cursor);
    if value.cursor.is_some() && !entry_cursor {
        return Ok(None);
    }
    let page = control
        .read_entry_page(value.max_bytes, value.cursor.as_deref())
        .await?;
    let Some(page) = page else {
        if !entry_cursor {
            return Ok(None);
        }
        return Ok(Some(stale_entry_cursor_json()));
    };
    match page {
        EntryPageRead::Unsupported => Ok(None),
        EntryPageRead::Error { error } => Ok(Some(json!({ "kind": "error", "error": error }))),
        EntryPageRead::Ok {
            messages,
            truncated,
            next_cursor,
        } => {
            let items: Vec<_> = messages
                .iter()
                .enumerate()
                .map(|(i, m)| read_item(m, i))
                .collect();
            let mut obj = json!({
                "kind": "ok",
                "thread": control.thread_id(),
                "link": thread_link(control.thread_id(), Some(control.title())),
                "items": items,
                "truncated": truncated,
                "source": "live_host",
            });
            if let Some(cursor) = next_cursor
                && let Some(map) = obj.as_object_mut()
            {
                map.insert("next_cursor".into(), Value::String(cursor));
            }
            Ok(Some(obj))
        }
    }
}

fn stale_entry_cursor_json() -> Value {
    match stale_entry_cursor() {
        EntryPageRead::Error { error } => json!({ "kind": "error", "error": error }),
        _ => Value::Null,
    }
}

pub fn live_read_result(
    thread_id: &str,
    title: Option<&str>,
    messages: Vec<ThreadTranscriptEntry>,
    value: &ThreadReadInput,
) -> Value {
    match read_transcript(
        ThreadReadSourceRef::Live { entries: messages },
        &read_options(value),
    ) {
        ThreadTranscriptReadResult::Error(error) => json!({ "kind": "error", "error": error }),
        ThreadTranscriptReadResult::Ok(live) => {
            let items: Vec<_> = live
                .items
                .iter()
                .enumerate()
                .map(|(i, m)| read_item(m, i))
                .collect();
            let mut obj = json!({
                "kind": "ok",
                "thread": thread_id,
                "link": thread_link(thread_id, title),
                "items": items,
                "truncated": live.truncated,
                "source": live.source,
            });
            if let Some(cursor) = live.next_cursor
                && let Some(map) = obj.as_object_mut()
            {
                map.insert("next_cursor".into(), Value::String(cursor));
            }
            obj
        }
    }
}

async fn read_degraded(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    thread_id: &str,
    value: &ThreadReadInput,
) -> Value {
    let book = send_address_book_async(options, current, thread_id, true).await;
    let Some(entry) = book.into_iter().find(|c| c.thread_id == thread_id) else {
        return failure(
            "not_resumable",
            "The thread has no live owner.",
            "Retry when the target is live.",
            None,
        );
    };
    let Some(session_path) = entry.session_path.as_deref() else {
        return failure(
            "not_resumable",
            "The thread has no live owner.",
            "Retry when the target is live.",
            None,
        );
    };
    let durable = read_transcript(
        ThreadReadSourceRef::Jsonl {
            path: std::path::Path::new(session_path),
            live_host_present: false,
        },
        &read_options(value),
    );
    match durable {
        ThreadTranscriptReadResult::Error(error) => json!({ "kind": "error", "error": error }),
        ThreadTranscriptReadResult::Ok(read) => {
            let messages: Vec<ThreadTranscriptEntry> = read
                .items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) != Some("message") {
                        return None;
                    }
                    let msg = item.get("message")?.as_object()?;
                    let role = msg.get("role").and_then(Value::as_str)?;
                    if matches!(role, "user" | "assistant" | "toolResult") {
                        Some(msg.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let items: Vec<_> = messages
                .iter()
                .enumerate()
                .map(|(i, m)| read_item(m, i))
                .collect();
            let mut obj = json!({
                "kind": "ok",
                "thread": thread_id,
                "link": thread_link(thread_id, entry.title.as_deref()),
                "items": items,
                "truncated": read.truncated,
                "source": read.source,
                "source_incomplete": read.source_incomplete,
            });
            if let Some(map) = obj.as_object_mut() {
                if let Some(cursor) = read.next_cursor {
                    map.insert("next_cursor".into(), Value::String(cursor));
                }
                if let Some(note) = entry.error_note {
                    map.insert("error_note".into(), Value::String(note));
                }
            }
            obj
        }
    }
}
