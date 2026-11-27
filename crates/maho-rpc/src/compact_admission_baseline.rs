//! senpi `modes/rpc/compact-admission-baseline.ts`: delivers the admission reply first,
//! then a `recovery_manifest` checkpoint identity, in one installed actor barrier.
//!
//! Also hosts `GeneratedFramesCursor` / `LegacyViewCursor` (matching `legacy-view-cursor.ts`)
//! shared with `compact_legacy_view`.

use crate::compact_recovery_types::EventLease;
use crate::jsonl::serialize_json_line;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

/// Pull-based async cursor of serialized JSONL frame strings, matching `LegacyViewCursor`.
pub trait LegacyViewCursor: Send {
    fn is_empty(&self) -> bool;
    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
    fn release(&mut self);
}

pub type FrameStreamFactory = Box<dyn FnOnce() -> BoxStream<'static, String> + Send>;

pub struct GeneratedFramesCursor {
    empty: bool,
    factory: Option<FrameStreamFactory>,
    stream: Option<BoxStream<'static, String>>,
    on_release: Option<Box<dyn FnOnce() + Send>>,
    released: bool,
}

impl GeneratedFramesCursor {
    pub fn new(
        empty: bool,
        factory: impl FnOnce() -> BoxStream<'static, String> + Send + 'static,
        on_release: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            empty,
            factory: Some(Box::new(factory)),
            stream: None,
            on_release: Some(Box::new(on_release)),
            released: false,
        }
    }
}

impl LegacyViewCursor for GeneratedFramesCursor {
    fn is_empty(&self) -> bool {
        self.empty
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async move {
            if self.released {
                return None;
            }
            if self.stream.is_none()
                && let Some(factory) = self.factory.take()
            {
                self.stream = Some(factory());
            }
            let next = match &mut self.stream {
                Some(s) => s.next().await,
                None => None,
            };
            if next.is_none() {
                self.release();
            }
            next
        })
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.factory = None;
        self.stream = None;
        if let Some(cb) = self.on_release.take() {
            cb();
        }
    }
}

#[derive(Debug, Clone)]
pub struct BaselineIdentity {
    pub session_id: String,
    pub admission_id: String,
    pub generation: u64,
    pub token: Option<String>,
    pub event_lease: Option<EventLease>,
    pub component_ref: Option<String>,
}

pub type BaselineReplyFuture = Pin<Box<dyn Future<Output = Option<Map<String, Value>>> + Send>>;

/// Identity first, then a pageable checkpoint identity, in one installed actor barrier.
pub fn admission_baseline(
    reply: BaselineReplyFuture,
    identity: BaselineIdentity,
    live: Arc<dyn Fn() -> bool + Send + Sync>,
    routes: Option<Vec<String>>,
) -> GeneratedFramesCursor {
    let reply_slot = Arc::new(Mutex::new(Some(reply)));
    GeneratedFramesCursor::new(
        false,
        move || {
            stream::once(async move {
                let fut = reply_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let Some(fut) = fut else {
                    return Vec::new();
                };
                let Some(answer) = fut.await else {
                    return Vec::new();
                };
                let mut lines = Vec::new();
                if let Ok(line) = serialize_json_line(&Value::Object(answer)) {
                    lines.push(line);
                }
                if live() {
                    let targets = routes.unwrap_or_else(|| vec![identity.admission_id.clone()]);
                    let comp_ref = identity
                        .component_ref
                        .clone()
                        .unwrap_or_else(|| "manifest".into());
                    for admission_id in targets {
                        let mut manifest = json!({
                            "type": "recovery_manifest",
                            "sessionId": identity.session_id,
                            "admissionId": admission_id,
                            "generation": identity.generation,
                            "componentRef": comp_ref,
                        });
                        if let Some(ref token) = identity.token {
                            manifest["token"] = Value::String(token.clone());
                        }
                        if let Some(ref lease) = identity.event_lease
                            && let Ok(v) = serde_json::to_value(lease)
                        {
                            manifest["eventLease"] = v;
                        }
                        if let Ok(line) = serialize_json_line(&manifest) {
                            lines.push(line);
                        }
                    }
                }
                lines
            })
            .flat_map(stream::iter)
            .boxed()
        },
        || {},
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emits_reply_then_recovery_manifests_for_routes() {
        let mut ans = Map::new();
        ans.insert("type".into(), json!("response"));
        ans.insert("success".into(), json!(true));
        let mut cursor = admission_baseline(
            Box::pin(async move { Some(ans) }),
            BaselineIdentity {
                session_id: "s1".into(),
                admission_id: "a1".into(),
                generation: 2,
                token: Some("tok-1".into()),
                event_lease: None,
                component_ref: None,
            },
            Arc::new(|| true),
            Some(vec!["a1".into(), "a2".into()]),
        );

        let mut frames = Vec::new();
        while let Some(frame) = cursor.next_frame().await {
            frames.push(frame);
        }
        assert_eq!(frames.len(), 3);
        let f0: Value = serde_json::from_str(&frames[0]).expect("json0");
        let f1: Value = serde_json::from_str(&frames[1]).expect("json1");
        let f2: Value = serde_json::from_str(&frames[2]).expect("json2");
        assert_eq!(f0["type"], "response");
        assert_eq!(f1["type"], "recovery_manifest");
        assert_eq!(f1["admissionId"], "a1");
        assert_eq!(f1["componentRef"], "manifest");
        assert_eq!(f2["admissionId"], "a2");
    }

    #[tokio::test]
    async fn emits_nothing_when_reply_is_none() {
        let mut cursor = admission_baseline(
            Box::pin(async { None }),
            BaselineIdentity {
                session_id: "s1".into(),
                admission_id: "a1".into(),
                generation: 1,
                token: None,
                event_lease: None,
                component_ref: None,
            },
            Arc::new(|| true),
            None,
        );
        assert!(cursor.next_frame().await.is_none());
    }
}
