use crate::turn_trace_bridge::is_traceparent;
use crate::types::{TurnSpanAttributes, TurnSpanStart, TurnSpanStatus, TurnTracing};
use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

pub const TURN_SPAN_NAME: &str = "omo.turn";

thread_local! {
    static ACTIVE: RefCell<Option<String>> = const { RefCell::new(None) };
}
tokio::task_local! { static ASYNC_ACTIVE: String; }

pub fn active_turn_traceparent() -> Option<String> {
    ACTIVE
        .with(|active| active.borrow().clone())
        .or_else(|| ASYNC_ACTIVE.try_with(Clone::clone).ok())
}

#[derive(Debug, Clone)]
pub struct TurnSpanRecord {
    pub name: &'static str,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub kind: u8,
    pub flags: u8,
    pub attributes: TurnSpanAttributes,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: Option<TurnSpanStatus>,
}

pub type TurnSpanSink = Arc<dyn Fn(TurnSpanRecord) + Send + Sync>;

#[derive(Clone)]
pub struct TurnSpanHandle {
    pub trace_id: String,
    pub span_id: String,
    pub traceparent: String,
    record: Arc<Mutex<TurnSpanRecord>>,
    sink: TurnSpanSink,
}

impl TurnSpanHandle {
    /// Synchronous context scope, restored even if the callback unwinds.
    pub fn run<T>(&self, callback: impl FnOnce() -> T) -> T {
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
            }
        }
        let _restore =
            Restore(ACTIVE.with(|active| active.replace(Some(self.traceparent.clone()))));
        callback()
    }

    /// Task-local context retained across awaits; concurrent turns remain isolated.
    pub async fn run_async<F: Future>(&self, future: F) -> F::Output {
        ASYNC_ACTIVE.scope(self.traceparent.clone(), future).await
    }

    pub fn set_attributes(&self, attributes: TurnSpanAttributes) {
        self.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attributes
            .extend(attributes);
    }

    pub fn end(&self, status: TurnSpanStatus) {
        let record = {
            let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
            if record.ended_at.is_some() {
                return;
            }
            record.status = Some(status);
            record.ended_at = Some(Utc::now());
            record.clone()
        };
        (self.sink)(record);
    }
}

pub struct TurnTracer {
    is_enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    sink: TurnSpanSink,
}

pub fn create_turn_tracing(
    is_enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    sink: TurnSpanSink,
) -> TurnTracer {
    TurnTracer { is_enabled, sink }
}

impl TurnTracing for TurnTracer {
    fn start_turn(&self, start: TurnSpanStart) -> Option<TurnSpanHandle> {
        if !(self.is_enabled)() {
            return None;
        }
        let parent = start
            .parent_traceparent
            .filter(|parent| is_traceparent(parent))
            .or_else(active_turn_traceparent);
        let trace_id = parent
            .as_ref()
            .map(|value| value[3..35].to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let span_id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
        let traceparent = format!("00-{trace_id}-{span_id}-01");
        let record = TurnSpanRecord {
            name: TURN_SPAN_NAME,
            trace_id: trace_id.clone(),
            span_id: span_id.clone(),
            parent_span_id: parent.map(|value| value[36..52].to_string()),
            kind: 1,
            flags: 1,
            attributes: start.attributes,
            started_at: Utc::now(),
            ended_at: None,
            status: None,
        };
        Some(TurnSpanHandle {
            trace_id,
            span_id,
            traceparent,
            record: Arc::new(Mutex::new(record)),
            sink: self.sink.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_unsampled_parent_is_recorded_and_end_is_idempotent() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let output = records.clone();
        let tracing = create_turn_tracing(
            Arc::new(|| true),
            Arc::new(move |record| output.lock().expect("records").push(record)),
        );
        let parent = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00";
        let turn = tracing
            .start_turn(TurnSpanStart {
                parent_traceparent: Some(parent.into()),
                ..Default::default()
            })
            .expect("turn");
        assert_eq!(turn.trace_id, &parent[3..35]);
        turn.run(|| assert_eq!(active_turn_traceparent(), Some(turn.traceparent.clone())));
        assert_eq!(active_turn_traceparent(), None);
        turn.end(TurnSpanStatus::Error);
        turn.end(TurnSpanStatus::Ok);
        let records = records.lock().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].parent_span_id.as_deref(), Some(&parent[36..52]));
        assert_eq!(records[0].status, Some(TurnSpanStatus::Error));
    }

    #[tokio::test]
    async fn concurrent_async_scopes_remain_isolated() {
        let tracing = create_turn_tracing(Arc::new(|| true), Arc::new(|_| {}));
        let first = tracing.start_turn(TurnSpanStart::default()).expect("first");
        let second = tracing
            .start_turn(TurnSpanStart::default())
            .expect("second");
        let (send, receive) = tokio::sync::oneshot::channel();
        let first_run = first.run_async(async {
            receive.await.expect("gate");
            assert_eq!(active_turn_traceparent(), Some(first.traceparent.clone()));
        });
        let second_run = second.run_async(async {
            send.send(()).expect("open");
            assert_eq!(active_turn_traceparent(), Some(second.traceparent.clone()));
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(first_run, second_run);
        })
        .await
        .expect("completion");
        assert_ne!(first.span_id, second.span_id);
        assert_eq!(active_turn_traceparent(), None);
    }

    #[test]
    fn disabled_and_malformed_parent() {
        assert!(
            create_turn_tracing(Arc::new(|| false), Arc::new(|_| {}))
                .start_turn(TurnSpanStart::default())
                .is_none()
        );
        let tracing = create_turn_tracing(
            Arc::new(|| true),
            Arc::new(|record| assert_eq!(record.parent_span_id, None)),
        );
        tracing
            .start_turn(TurnSpanStart {
                parent_traceparent: Some("invalid".into()),
                ..Default::default()
            })
            .expect("turn")
            .end(TurnSpanStatus::Ok);
    }
    #[test]
    fn active_parent_and_attribute_updates_reach_the_sink() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let output = records.clone();
        let tracing = create_turn_tracing(
            Arc::new(|| true),
            Arc::new(move |record| output.lock().expect("records").push(record)),
        );
        let parent = tracing
            .start_turn(TurnSpanStart::default())
            .expect("parent");
        let child = parent
            .run(|| tracing.start_turn(TurnSpanStart::default()))
            .expect("child");
        assert_eq!(parent.trace_id, child.trace_id);
        assert_ne!(parent.span_id, child.span_id);
        child.set_attributes(std::collections::BTreeMap::from([(
            "gen_ai.response.model".into(),
            crate::types::TurnSpanAttribute::String("gpt-5.6-sol".into()),
        )]));
        child.end(TurnSpanStatus::Ok);
        let records = records.lock().expect("records");
        assert_eq!(records[0].name, TURN_SPAN_NAME);
        assert_eq!(
            records[0].parent_span_id.as_deref(),
            Some(parent.span_id.as_str())
        );
        assert_eq!(records[0].kind, 1);
        assert_eq!(records[0].flags, 1);
        assert!(records[0].attributes.contains_key("gen_ai.response.model"));
        assert!(records[0].ended_at.expect("ended") >= records[0].started_at);
    }
}
