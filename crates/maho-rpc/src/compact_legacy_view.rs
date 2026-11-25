//! senpi `modes/rpc/compact-legacy-view.ts`: builds legacy attach baselines and
//! `resync_required` resume views from frozen compact cuts under checkpoint holds (R3.C1.5).

use crate::compact_admission_baseline::GeneratedFramesCursor;
#[cfg(test)]
use crate::compact_admission_baseline::LegacyViewCursor;
use crate::compact_current_components::{CompactDialog, CurrentComponentsView, DialogAudience};
use crate::compact_recovery_types::{RecoveryOutcome, RefusalReason, refusal_reason};
use crate::jsonl::serialize_json_line;
use crate::media_placeholders::omit_inline_media;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Streamed block phase in a frozen active-message cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveBlockPhase {
    Initial,
    Open,
    Ended,
}

/// Active content block view from a frozen cut (`compact-state-view.ts` `ActiveBlockView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveBlockView {
    pub content_index: usize,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ActiveBlockPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Value>,
}

/// Active message view from a frozen cut (`compact-state-view.ts` `ActiveMessageView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMessageView {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Map<String, Value>>,
    pub blocks: Vec<ActiveBlockView>,
}

/// Call-component projection row for one tool call (`compact-call-model.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallViewRow {
    pub owning_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_name: Option<String>,
}

/// Calls component view from a frozen cut.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenCallsView {
    pub calls: Vec<CallViewRow>,
}

/// Legacy `resync_required` active-message resume plus follow-up tool reset frames
/// (`legacy-resync-view.ts` `ResyncResume`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResyncResume {
    pub message: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Value>,
}

/// Snapshot read from a held frozen cut for legacy attach or resync delivery.
#[derive(Debug, Clone)]
pub struct LegacyViewSnapshot {
    pub components: Option<CurrentComponentsView>,
    pub active_message: Option<ActiveMessageView>,
    pub calls: Option<FrozenCallsView>,
}

/// A lane and the root it froze for one legacy view (R3.C1.5). Every read of the root runs inside
/// its checkpoint hold so a reclaim waits for it instead of releasing the root under it.
pub trait LegacyViewSource: Send + Sync {
    fn recovery_epoch(&self) -> u64;
    fn is_empty_attach(&self) -> bool;
    fn holds_root(&self) -> bool;
    fn read_attach(&self) -> BoxFuture<'_, Result<LegacyViewSnapshot, String>>;
    fn read_resync(&self) -> BoxFuture<'_, Result<Option<ActiveMessageView>, String>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyAttachView {
    pub placeholders: bool,
    pub now: u64,
}

/// Which refused cuts the resync reports on stderr: only a store that could not serve the cut.
pub const fn is_reported_refusal(reason: RefusalReason) -> bool {
    match reason {
        RefusalReason::StorageUnavailable => true,
        RefusalReason::UnknownToken
        | RefusalReason::Released
        | RefusalReason::LaneRetired
        | RefusalReason::StaleEpoch
        | RefusalReason::PeerGone
        | RefusalReason::SessionClosed
        | RefusalReason::SupersededToken => false,
    }
}

fn unavailable(session_id: &str, cause: &str) {
    eprintln!("senpi rpc legacy view: session {session_id} current view unavailable: {cause}");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Current,
    Start,
}

fn block_value(block: &ActiveBlockView, layout: Layout) -> Value {
    let value = block.block.clone().unwrap_or_else(|| json!({}));
    if layout == Layout::Current || matches!(block.phase, None | Some(ActiveBlockPhase::Initial)) {
        return value;
    }
    if let Some(mut map) = value.as_object().cloned() {
        if map.contains_key("text") {
            map.insert("text".into(), Value::String(String::new()));
            return Value::Object(map);
        }
        if map.contains_key("thinking") {
            map.insert("thinking".into(), Value::String(String::new()));
            return Value::Object(map);
        }
    }
    value
}

fn legacy_message(message: &ActiveMessageView, layout: Layout) -> Option<Map<String, Value>> {
    let fields = message.fields.as_ref()?;
    if fields.contains_key("content") {
        return Some(fields.clone());
    }
    let mut out = Map::new();
    if let Some(role) = fields.get("role") {
        out.insert("role".into(), role.clone());
    }
    let max_idx = message
        .blocks
        .iter()
        .map(|b| b.content_index)
        .max()
        .map_or(0, |m| m + 1);
    let mut content = vec![Value::Null; max_idx];
    for block in &message.blocks {
        content[block.content_index] = block_value(block, layout);
    }
    out.insert("content".into(), Value::Array(content));
    for (k, v) in fields {
        if k != "role" {
            out.insert(k.clone(), v.clone());
        }
    }
    Some(out)
}

type ResolvedNames = HashMap<usize, String>;

fn resolved_names(calls: &FrozenCallsView, message_id: &str) -> ResolvedNames {
    let mut names = HashMap::new();
    for call in &calls.calls {
        if call.owning_message_id == message_id
            && let (Some(idx), Some(resolved)) = (call.content_index, &call.resolved_name)
        {
            names.insert(idx, resolved.clone());
        }
    }
    names
}

fn block_frames(
    block: &ActiveBlockView,
    usage: Option<&Value>,
    resolved: &ResolvedNames,
) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(phase) = block.phase else {
        return out;
    };
    if phase == ActiveBlockPhase::Initial {
        return out;
    }
    let Some(ref raw_block) = block.block else {
        return out;
    };
    let Some(block_type) = raw_block.get("type").and_then(Value::as_str) else {
        return out;
    };
    let ended = phase == ActiveBlockPhase::Ended;
    let resolved_tool_name = resolved.get(&block.content_index).cloned();
    let make_update = |assistant_event: Value, named: bool| -> Value {
        let mut map = Map::new();
        map.insert("type".into(), json!("message_update"));
        if let Some(u) = usage {
            map.insert("usage".into(), u.clone());
        }
        map.insert("assistantMessageEvent".into(), assistant_event);
        if named && let Some(ref name) = resolved_tool_name {
            map.insert("resolvedToolName".into(), json!(name));
        }
        Value::Object(map)
    };

    match block_type {
        "text" | "thinking" => {
            out.push(make_update(
                json!({"type": format!("{block_type}_start"), "contentIndex": block.content_index}),
                false,
            ));
            if !block.text.is_empty() {
                out.push(make_update(
                    json!({
                        "type": format!("{block_type}_delta"),
                        "contentIndex": block.content_index,
                        "delta": block.text,
                    }),
                    false,
                ));
            }
            if ended {
                out.push(make_update(
                    json!({
                        "type": format!("{block_type}_end"),
                        "contentIndex": block.content_index,
                        "content": block.text,
                    }),
                    false,
                ));
            }
        }
        "toolCall" => {
            let id = raw_block.get("id").cloned().unwrap_or(Value::Null);
            let tool_name = raw_block.get("name").cloned().unwrap_or(Value::Null);
            out.push(make_update(
                json!({
                    "type": "toolcall_start",
                    "contentIndex": block.content_index,
                    "id": id,
                    "toolName": tool_name,
                }),
                true,
            ));
            if !block.text.is_empty() {
                out.push(make_update(
                    json!({
                        "type": "toolcall_delta",
                        "contentIndex": block.content_index,
                        "delta": block.text,
                    }),
                    false,
                ));
            }
            if ended {
                out.push(make_update(
                    json!({
                        "type": "toolcall_end",
                        "contentIndex": block.content_index,
                        "toolCall": raw_block,
                    }),
                    true,
                ));
            }
        }
        _ => {}
    }
    out
}

/// RULING-115: the cut carries a tool call's parsed arguments only, so each call still open at it is
/// reset and resumes from offset 0 with its whole raw prefix.
pub fn tool_reset_frames(message: &ActiveMessageView) -> Vec<Value> {
    let usage = message.fields.as_ref().and_then(|f| f.get("usage"));
    let mut out = Vec::new();
    for block in &message.blocks {
        if block.phase != Some(ActiveBlockPhase::Open) {
            continue;
        }
        let is_tool = block
            .block
            .as_ref()
            .and_then(|b| b.get("type"))
            .and_then(Value::as_str)
            == Some("toolCall");
        if !is_tool {
            continue;
        }
        out.push(json!({
            "type": "message_tool_arguments_reset",
            "contentIndex": block.content_index,
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
        }));
        if !block.text.is_empty() {
            let mut map = Map::new();
            map.insert("type".into(), json!("message_update"));
            if let Some(u) = usage {
                map.insert("usage".into(), u.clone());
            }
            map.insert(
                "assistantMessageEvent".into(),
                json!({
                    "type": "toolcall_delta",
                    "contentIndex": block.content_index,
                    "delta": block.text,
                }),
            );
            out.push(Value::Object(map));
        }
    }
    out
}

fn message_frames(message: &ActiveMessageView, resolved: &ResolvedNames) -> Vec<Value> {
    let Some(start) = legacy_message(message, Layout::Start) else {
        return Vec::new();
    };
    let is_assistant = start.get("role").and_then(Value::as_str) == Some("assistant");
    let mut out = vec![json!({
        "type": "message_start",
        "message": Value::Object(start),
    })];
    if !is_assistant {
        return out;
    }
    let usage = message.fields.as_ref().and_then(|f| f.get("usage"));
    for block in &message.blocks {
        out.extend(block_frames(block, usage, resolved));
    }
    out
}

fn remaining_ms(dialog: &CompactDialog, now: u64) -> u64 {
    dialog
        .deadline_at_ms
        .map_or(0, |deadline| deadline.saturating_sub(now))
}

fn encode_frame(frame: Value, placeholders: bool) -> Option<String> {
    let serialized = if placeholders {
        let omitted = omit_inline_media(&frame);
        serialize_json_line(omitted.as_ref())
    } else {
        serialize_json_line(&frame)
    };
    serialized.ok()
}

fn tag_session(frame: &Value, session_id: &str) -> Option<Value> {
    let mut map = frame.as_object()?.clone();
    map.insert("sessionId".into(), json!(session_id));
    Some(Value::Object(map))
}

fn attach_stream(
    session_id: String,
    source: Arc<dyn LegacyViewSource>,
    view: LegacyAttachView,
    released: Arc<AtomicBool>,
) -> BoxStream<'static, String> {
    let epoch = source.recovery_epoch();
    Box::pin(
        stream::once(async move {
            let cancelled = || {
                released.load(Ordering::SeqCst)
                    || !source.holds_root()
                    || source.recovery_epoch() != epoch
            };
            let snapshot = match source.read_attach().await {
                Ok(snap) => snap,
                Err(cause) => {
                    if !cancelled() {
                        unavailable(&session_id, &cause);
                    }
                    return stream::iter(Vec::<String>::new()).boxed();
                }
            };
            let mut lines = Vec::new();
            if let Some(ref components) = snapshot.components {
                if let Some(ref lifecycle) = components.lifecycle
                    && let Some(tagged) = tag_session(lifecycle, &session_id)
                    && let Some(line) = encode_frame(tagged, view.placeholders)
                {
                    lines.push(line);
                }
                if let Some(ref message) = snapshot.active_message {
                    let resolved = snapshot
                        .calls
                        .as_ref()
                        .map(|c| resolved_names(c, &message.message_id))
                        .unwrap_or_default();
                    for frame in message_frames(message, &resolved) {
                        if let Some(tagged) = tag_session(&frame, &session_id)
                            && let Some(line) = encode_frame(tagged, view.placeholders)
                        {
                            lines.push(line);
                        }
                    }
                }
                for dialog in &components.dialogs {
                    if dialog.audience == DialogAudience::Broadcast
                        && let Some(ref request) = dialog.request
                        && let Some(mut tagged) = tag_session(request, &session_id)
                    {
                        if let Some(obj) = tagged.as_object_mut() {
                            obj.insert("remainingMs".into(), json!(remaining_ms(dialog, view.now)));
                        }
                        if let Some(line) = encode_frame(tagged, view.placeholders) {
                            lines.push(line);
                        }
                    }
                }
            }
            stream::iter(lines).boxed()
        })
        .flatten(),
    )
}

/// A late peer's attach baseline from the lane's frozen view; a source without a readable cut is
/// empty.
pub fn legacy_attach_view(
    session_id: impl Into<String>,
    source: Option<Arc<dyn LegacyViewSource>>,
    view: LegacyAttachView,
    on_release: impl FnOnce() + Send + 'static,
) -> GeneratedFramesCursor {
    let session_id = session_id.into();
    let empty = source.as_ref().is_none_or(|s| s.is_empty_attach());
    let released = Arc::new(AtomicBool::new(false));
    let rel_flag = released.clone();
    let on_release_slot: Mutex<Option<Box<dyn FnOnce() + Send>>> =
        Mutex::new(Some(Box::new(on_release)));

    GeneratedFramesCursor::new(
        empty,
        move || match source {
            Some(src) => attach_stream(session_id, src, view, rel_flag),
            None => Box::pin(stream::empty()),
        },
        move || {
            released.store(true, Ordering::SeqCst);
            if let Some(cb) = on_release_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                cb();
            }
        },
    )
}

/// The message slot of a session's `resync_required` frame: C's active message, or null when it
/// has none. A cut the host could not serve (`storage_unavailable`) is reported once; a retired
/// lane has nothing to report.
pub async fn legacy_resync_message(
    session_id: &str,
    source: Option<RecoveryOutcome<Arc<dyn LegacyViewSource>>>,
) -> ResyncResume {
    let Some(outcome) = source else {
        return ResyncResume {
            message: None,
            frames: Vec::new(),
        };
    };
    let source = match outcome {
        Ok(src) => src,
        Err(failure) => {
            if is_reported_refusal(refusal_reason(&failure)) {
                unavailable(session_id, failure.reason_str());
            }
            return ResyncResume {
                message: None,
                frames: Vec::new(),
            };
        }
    };
    match source.read_resync().await {
        Ok(Some(message)) => match legacy_message(&message, Layout::Current) {
            Some(current) => ResyncResume {
                message: Some(Value::Object(current)),
                frames: tool_reset_frames(&message),
            },
            None => ResyncResume {
                message: None,
                frames: Vec::new(),
            },
        },
        Ok(None) => ResyncResume {
            message: None,
            frames: Vec::new(),
        },
        Err(cause) => {
            if source.holds_root() {
                unavailable(session_id, &cause);
            }
            ResyncResume {
                message: None,
                frames: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_current_components::{CompactRunState, DialogCounts};
    use crate::compact_recovery_types::{StorageUnavailableReason, storage_unavailable};
    use std::sync::atomic::AtomicUsize;

    struct MockSource {
        snapshot: Result<LegacyViewSnapshot, String>,
        resync: Result<Option<ActiveMessageView>, String>,
        holds: bool,
    }

    impl LegacyViewSource for MockSource {
        fn recovery_epoch(&self) -> u64 {
            1
        }
        fn is_empty_attach(&self) -> bool {
            false
        }
        fn holds_root(&self) -> bool {
            self.holds
        }
        fn read_attach(&self) -> BoxFuture<'_, Result<LegacyViewSnapshot, String>> {
            let res = self.snapshot.clone();
            Box::pin(async move { res })
        }
        fn read_resync(&self) -> BoxFuture<'_, Result<Option<ActiveMessageView>, String>> {
            let res = self.resync.clone();
            Box::pin(async move { res })
        }
    }

    #[test]
    fn reported_refusal_only_flags_storage_unavailable() {
        assert!(is_reported_refusal(RefusalReason::StorageUnavailable));
        assert!(!is_reported_refusal(RefusalReason::LaneRetired));
        assert!(!is_reported_refusal(RefusalReason::UnknownToken));
        assert!(!is_reported_refusal(RefusalReason::SupersededToken));
    }

    #[tokio::test]
    async fn legacy_attach_view_emits_lifecycle_active_message_and_broadcast_dialogs() {
        let mut fields = Map::new();
        fields.insert("role".into(), json!("assistant"));
        let active = ActiveMessageView {
            message_id: "m1".into(),
            fields: Some(fields),
            blocks: vec![
                ActiveBlockView {
                    content_index: 0,
                    text: "hello".into(),
                    phase: Some(ActiveBlockPhase::Open),
                    block: Some(json!({"type": "text", "text": "hello"})),
                },
                ActiveBlockView {
                    content_index: 1,
                    text: "{\"a\":1}".into(),
                    phase: Some(ActiveBlockPhase::Ended),
                    block: Some(json!({"type": "toolCall", "id": "c1", "name": "bash"})),
                },
            ],
        };
        let calls = FrozenCallsView {
            calls: vec![CallViewRow {
                owning_message_id: "m1".into(),
                content_index: Some(1),
                resolved_name: Some("bash_resolved".into()),
            }],
        };
        let src = Arc::new(MockSource {
            snapshot: Ok(LegacyViewSnapshot {
                components: Some(CurrentComponentsView {
                    run_state: CompactRunState::Running,
                    lifecycle: Some(json!({"type": "session_lifecycle", "state": "running"})),
                    dialogs: vec![CompactDialog {
                        id: "d1".into(),
                        audience: DialogAudience::Broadcast,
                        method: "question".into(),
                        deadline_at_ms: Some(1_500),
                        request: Some(json!({"type": "extension_ui_request", "id": "d1"})),
                    }],
                    dialog_counts: DialogCounts {
                        broadcast: 1,
                        targeted: 0,
                    },
                }),
                active_message: Some(active),
                calls: Some(calls),
            }),
            resync: Ok(None),
            holds: true,
        });

        let released = Arc::new(AtomicUsize::new(0));
        let rel_clone = released.clone();
        let mut cursor = legacy_attach_view(
            "s1",
            Some(src),
            LegacyAttachView {
                placeholders: false,
                now: 1_000,
            },
            move || {
                rel_clone.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(!cursor.is_empty());

        let mut frames = Vec::new();
        while let Some(line) = cursor.next_frame().await {
            let v: Value = serde_json::from_str(line.trim()).expect("json line");
            frames.push(v);
        }
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(frames[0]["type"], "session_lifecycle");
        assert_eq!(frames[0]["sessionId"], "s1");
        assert_eq!(frames[1]["type"], "message_start");
        assert_eq!(frames[1]["message"]["content"][0]["text"], "");
        assert_eq!(
            frames.last().expect("dialog frame")["remainingMs"],
            json!(500)
        );
    }

    #[tokio::test]
    async fn legacy_resync_message_emits_tool_reset_frames_for_open_tool_calls() {
        let mut fields = Map::new();
        fields.insert("role".into(), json!("assistant"));
        let src: Arc<dyn LegacyViewSource> = Arc::new(MockSource {
            snapshot: Err("unused".into()),
            resync: Ok(Some(ActiveMessageView {
                message_id: "m1".into(),
                fields: Some(fields),
                blocks: vec![ActiveBlockView {
                    content_index: 0,
                    text: "{\"cmd\":".into(),
                    phase: Some(ActiveBlockPhase::Open),
                    block: Some(json!({"type": "toolCall", "id": "t1", "name": "bash"})),
                }],
            })),
            holds: true,
        });
        let resume = legacy_resync_message("s1", Some(Ok(src))).await;
        assert!(resume.message.is_some());
        assert_eq!(resume.frames.len(), 2);
        assert_eq!(resume.frames[0]["type"], "message_tool_arguments_reset");
        assert_eq!(
            resume.frames[1]["assistantMessageEvent"]["type"],
            "toolcall_delta"
        );

        let refused = legacy_resync_message(
            "s1",
            Some(Err(storage_unavailable(
                StorageUnavailableReason::CompactStateUnavailable,
            ))),
        )
        .await;
        assert_eq!(refused.message, None);
        assert!(refused.frames.is_empty());
    }
}
