//! senpi `modes/rpc/legacy-resync-view.ts`: a shed's cut producing one `resync_required` frame
//! per session (plus resume frames) in order when the gap is delivered.

use crate::jsonl::serialize_json_line;
use crate::legacy_view_cursor::{FrameStream, GeneratedFramesCursor, LegacyViewCursor};
use crate::media_placeholders::omit_inline_media;
use crate::socket_peer_recovery::{ResyncReason, RpcSessionResyncRequired};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct FrozenResyncView {
    pub reason: ResyncReason,
    pub placeholders: bool,
    /// Read when the cut is read: whether the attachment the cut was taken for is gone (`closed` then).
    pub closed_now: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub on_release: Option<Box<dyn FnOnce() + Send>>,
}

/// What one session resumes from: the message a legacy consumer holds at the cut (null when it
/// has nothing), and the frames that follow its `resync_required` before any captured record
/// (RULING-115: an open tool call's reset).
#[derive(Debug, Clone, Default)]
pub struct ResyncResume {
    pub message: Value,
    pub frames: Vec<Map<String, Value>>,
}

pub type ResyncMessageSource = Arc<dyn Fn(&str) -> BoxFuture<'static, ResyncResume> + Send + Sync>;

/// Cursor implementing `LegacyViewCursor` over a shed's per-session resync frames.
pub struct ResyncCursor {
    pub sessions: Vec<String>,
    inner: GeneratedFramesCursor,
}

impl ResyncCursor {
    pub fn new(
        sessions: Vec<String>,
        source: impl FnOnce() -> FrameStream + Send + 'static,
        on_release: impl FnOnce() + Send + 'static,
    ) -> Self {
        let empty = sessions.is_empty();
        Self {
            sessions,
            inner: GeneratedFramesCursor::new(empty, source, on_release),
        }
    }
}

#[async_trait]
impl LegacyViewCursor for ResyncCursor {
    fn empty(&mut self) -> bool {
        self.inner.empty()
    }

    async fn next(&mut self) -> Option<String> {
        self.inner.next().await
    }

    fn release(&mut self) {
        self.inner.release();
    }
}

fn resync_frames(
    entries: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    message_of: ResyncMessageSource,
    reason: ResyncReason,
    placeholders: bool,
    closed_now: Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> FrameStream {
    let sessions = entries();
    Box::pin(
        stream::iter(sessions)
            .then(move |session_id| {
                let message_of = Arc::clone(&message_of);
                let closed_now = Arc::clone(&closed_now);
                async move {
                    let closed = closed_now(&session_id);
                    let resume = if closed {
                        ResyncResume {
                            message: Value::Null,
                            frames: Vec::new(),
                        }
                    } else {
                        message_of(&session_id).await
                    };
                    let resync = RpcSessionResyncRequired {
                        session_id: session_id.clone(),
                        reason,
                        message: resume.message,
                        closed,
                    }
                    .to_value();
                    let mut lines = Vec::with_capacity(1 + resume.frames.len());
                    let mut all_frames = Vec::with_capacity(1 + resume.frames.len());
                    all_frames.push(resync);
                    for mut frame in resume.frames {
                        frame.insert("sessionId".into(), Value::String(session_id.clone()));
                        all_frames.push(Value::Object(frame));
                    }
                    for frame in all_frames {
                        let serialized = if placeholders {
                            let redacted = omit_inline_media(&frame);
                            serialize_json_line(redacted.as_ref())
                        } else {
                            serialize_json_line(&frame)
                        };
                        if let Ok(line) = serialized {
                            lines.push(line);
                        }
                    }
                    stream::iter(lines)
                }
            })
            .flatten(),
    )
}

/// A shed's cut: one `resync_required` frame per session, read in order when the gap is delivered.
/// `entries` fixes the sessions at the first read (the shed's set, or today's delivery-time union),
/// `message_of` each one's message.
pub fn resync_cut(
    sessions: Vec<String>,
    entries: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    message_of: ResyncMessageSource,
    mut view: FrozenResyncView,
    track: impl FnOnce(Box<dyn FnOnce() + Send>) -> Box<dyn FnOnce() + Send>,
) -> ResyncCursor {
    let reason = view.reason;
    let placeholders = view.placeholders;
    let closed_now = Arc::clone(&view.closed_now);
    let on_release = view.on_release.take().unwrap_or_else(|| Box::new(|| {}));
    let tracked = track(on_release);
    ResyncCursor::new(
        sessions,
        move || resync_frames(entries, message_of, reason, placeholders, closed_now),
        tracked,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn emits_resync_required_with_closed_and_placeholders() {
        let released = Arc::new(AtomicUsize::new(0));
        let released_clone = Arc::clone(&released);
        let mut cursor = resync_cut(
            vec!["s1".into(), "s2".into()],
            Arc::new(|| vec!["s1".into(), "s2".into()]),
            Arc::new(|sid| {
                let sid = sid.to_owned();
                Box::pin(async move {
                    let mut extra = Map::new();
                    extra.insert("type".into(), json!("toolcall_reset"));
                    ResyncResume {
                        message: json!({
                            "role": "toolResult",
                            "toolCallId": format!("call-{sid}"),
                            "content": [{"type": "image", "data": "YQ==", "mimeType": "image/png"}]
                        }),
                        frames: vec![extra],
                    }
                })
            }),
            FrozenResyncView {
                reason: ResyncReason::Overflow,
                placeholders: true,
                closed_now: Arc::new(|sid| sid == "s2"),
                on_release: Some(Box::new(move || {
                    released_clone.fetch_add(1, Ordering::SeqCst);
                })),
            },
            |cb| cb,
        );
        assert!(!cursor.empty());
        let first: Value =
            serde_json::from_str(cursor.next().await.expect("first").trim_end()).expect("json");
        assert_eq!(first["type"], "resync_required");
        assert_eq!(first["sessionId"], "s1");
        assert_eq!(first["reason"], "overflow");
        assert_eq!(first["message"]["content"][0]["type"], "image_ref");
        let second: Value =
            serde_json::from_str(cursor.next().await.expect("second").trim_end()).expect("json");
        assert_eq!(second["type"], "toolcall_reset");
        assert_eq!(second["sessionId"], "s1");
        let third: Value =
            serde_json::from_str(cursor.next().await.expect("third").trim_end()).expect("json");
        assert_eq!(third["sessionId"], "s2");
        assert_eq!(third["closed"], true);
        assert_eq!(third["message"], Value::Null);
        assert_eq!(cursor.next().await, None);
        cursor.release();
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }
}
