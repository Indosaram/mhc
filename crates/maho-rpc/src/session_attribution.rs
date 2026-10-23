use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionAttribution {
    pub session_id: Option<String>,
    pub tool: Option<String>,
    pub job: Option<JobKind>,
}
/// Kind of non-tool job holding the loop (senpi#3566): `hook:<name>`, `fanout`, `session.parse`,
/// `compaction.prepare` or `history.spool`.
pub type JobKind = String;
/// One stall line's attribution clause: `sessionId=<id>[ tool=<name>][ job=<kind>]` (senpi `describeAttribution`).
pub fn describe_attribution(attribution: Option<&SessionAttribution>) -> String {
    let Some(attribution) =
        attribution.filter(|a| a.session_id.is_some() || a.tool.is_some() || a.job.is_some())
    else {
        return "no attributed session".into();
    };
    let tool = attribution
        .tool
        .as_deref()
        .filter(|tool| !tool.is_empty())
        .map(|tool| format!(" tool={tool}"))
        .unwrap_or_default();
    let job = attribution
        .job
        .as_deref()
        .filter(|job| !job.is_empty())
        .map(|job| format!(" job={job}"))
        .unwrap_or_default();
    format!(
        "sessionId={}{tool}{job}",
        attribution.session_id.as_deref().unwrap_or("unknown")
    )
}
type ActivityListener = Arc<dyn Fn() + Send + Sync>;
static ACTIVITY_START_LISTENERS: Mutex<Vec<(u64, ActivityListener)>> = Mutex::new(Vec::new());
static NEXT_LISTENER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Subscribe to the start of attributed work (senpi#3738): the loop-lag watchdog slows its tick while the host
/// is idle and uses this to return to its fast tick before the work that could stall the loop runs.
pub fn on_session_activity_start(
    listener: impl Fn() + Send + Sync + 'static,
) -> impl FnOnce() + Send + Sync {
    let id = NEXT_LISTENER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ACTIVITY_START_LISTENERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((id, Arc::new(listener) as ActivityListener));
    move || {
        ACTIVITY_START_LISTENERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(key, _)| *key != id)
    }
}
fn notify_activity_started() {
    let listeners = ACTIVITY_START_LISTENERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(_, listener)| Arc::clone(listener))
        .collect::<Vec<_>>();
    for listener in listeners {
        listener();
    }
}
tokio::task_local! {static ATTRIBUTION:SessionAttribution;}
pub fn current_session_attribution() -> Option<SessionAttribution> {
    ATTRIBUTION.try_with(Clone::clone).ok()
}
pub async fn run_with_session_attribution<T>(
    registry: &SessionActivityRegistry,
    attribution: SessionAttribution,
    task: impl std::future::Future<Output = T>,
) -> T {
    let _span = registry.open_span(attribution.clone(), None); // open_span notifies the activity-start listeners synchronously
    ATTRIBUTION.scope(attribution, task).await
}
#[derive(Default)]
struct ActivityState {
    sequence: u64,
    open: BTreeMap<u64, SessionAttribution>,
    last_finished: Option<(u64, SessionAttribution)>,
}
#[derive(Clone, Default)]
pub struct SessionActivityRegistry(Arc<Mutex<ActivityState>>);
impl SessionActivityRegistry {
    pub fn mark(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sequence
    }
    pub fn since(&self, mark: u64) -> Option<SessionAttribution> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((sequence, attribution)) = &state.last_finished
            && *sequence > mark
        {
            return Some(attribution.clone());
        }
        state
            .open
            .last_key_value()
            .map(|(_, attribution)| attribution.clone())
    }
    pub fn open_span(
        &self,
        attribution: SessionAttribution,
        ambient: Option<&SessionAttribution>,
    ) -> SessionAttributionSpan {
        // Only set fields are kept; the session id falls back to the ambient one.
        let merged = SessionAttribution {
            session_id: attribution
                .session_id
                .or_else(|| ambient.and_then(|ambient| ambient.session_id.clone())),
            tool: attribution.tool,
            job: attribution.job,
        };
        let mark = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.sequence += 1;
            let mark = state.sequence;
            state.open.insert(mark, merged);
            mark
        };
        notify_activity_started();
        SessionAttributionSpan {
            registry: self.clone(),
            mark,
        }
    }
    /// A job-kind span that inherits the ambient session; it stays blamable until the span closes (senpi `openJobSpan`).
    pub fn open_job_span(&self, job: impl Into<JobKind>) -> SessionAttributionSpan {
        let ambient = current_session_attribution();
        self.open_span(
            SessionAttribution {
                session_id: None,
                tool: None,
                job: Some(job.into()),
            },
            ambient.as_ref(),
        )
    }
}
/// Run `task` as a job of `job`'s kind, attributed for its whole lifetime like a routed command (senpi `runWithJob`).
pub async fn run_with_job<T>(
    registry: &SessionActivityRegistry,
    job: impl Into<JobKind>,
    task: impl std::future::Future<Output = T>,
) -> T {
    let session_id = current_session_attribution().and_then(|ambient| ambient.session_id);
    run_with_session_attribution(
        registry,
        SessionAttribution {
            session_id,
            tool: None,
            job: Some(job.into()),
        },
        task,
    )
    .await
}
pub struct SessionAttributionSpan {
    registry: SessionActivityRegistry,
    mark: u64,
}
/// Open-ended attribution for work that outlives its dispatch (senpi `openSessionAttributionSpan`).
pub fn open_session_attribution_span(
    registry: &SessionActivityRegistry,
    attribution: SessionAttribution,
    ambient: Option<&SessionAttribution>,
) -> SessionAttributionSpan {
    registry.open_span(attribution, ambient)
}
/// Current activity counter the watchdog samples once per tick (senpi `sessionActivityMark`).
pub fn session_activity_mark(registry: &SessionActivityRegistry) -> u64 {
    registry.mark()
}
/// The activity to blame for work that ran after `mark` (senpi `sessionActivitySince`).
pub fn session_activity_since(
    registry: &SessionActivityRegistry,
    mark: u64,
) -> Option<SessionAttribution> {
    registry.since(mark)
}
/// Tool spans of one session, driven by the records its runtime emits (senpi `createToolAttributionSpans`).
pub fn create_tool_attribution_spans(
    session_id: String,
    registry: SessionActivityRegistry,
) -> ToolAttributionSpans {
    ToolAttributionSpans::new(session_id, registry)
}
impl SessionAttributionSpan {
    pub fn close(&mut self) {
        let mut state = self
            .registry
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(attribution) = state.open.remove(&self.mark) {
            state.sequence += 1;
            state.last_finished = Some((state.sequence, attribution));
        }
    }
}
impl Drop for SessionAttributionSpan {
    fn drop(&mut self) {
        self.close();
    }
}
pub struct ToolAttributionSpans {
    session_id: String,
    registry: SessionActivityRegistry,
    spans: Vec<(String, SessionAttributionSpan)>,
}
impl ToolAttributionSpans {
    pub fn new(session_id: String, registry: SessionActivityRegistry) -> Self {
        Self {
            session_id,
            registry,
            spans: vec![],
        }
    }
    pub fn close_all(&mut self) {
        self.spans.clear();
    }
    pub fn observe(&mut self, record: &serde_json::Value) {
        let Some(kind) = record.get("type").and_then(serde_json::Value::as_str) else {
            return;
        };
        if matches!(kind, "tool_execution_start" | "tool_execution_end")
            && let Some(id) = record.get("toolCallId").and_then(serde_json::Value::as_str)
        {
            if let Some(index) = self.spans.iter().position(|(key, _)| key == id) {
                self.spans.remove(index);
            }
            if kind == "tool_execution_start" {
                let span = self.registry.open_span(
                    SessionAttribution {
                        session_id: Some(self.session_id.clone()),
                        tool: record
                            .get("toolName")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        job: None,
                    },
                    None,
                );
                self.spans.push((id.into(), span));
            }
            return;
        }
        if matches!(kind, "agent_settled" | "agent_idle") {
            self.close_all();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn async_context_and_activity_live_until_exact_completion() {
        let registry = SessionActivityRegistry::default();
        let attribution = SessionAttribution {
            session_id: Some("session".into()),
            tool: None,
            job: None,
        };
        let (mark_send, mark_recv) = tokio::sync::oneshot::channel();
        let (release_send, release_recv) = tokio::sync::oneshot::channel();
        let work = run_with_session_attribution(&registry, attribution.clone(), async {
            assert_eq!(current_session_attribution(), Some(attribution.clone()));
            mark_send.send(()).unwrap();
            release_recv.await.unwrap();
            assert_eq!(current_session_attribution(), Some(attribution.clone()));
        });
        let observer = async {
            mark_recv.await.unwrap();
            assert_eq!(registry.since(registry.mark()), Some(attribution.clone()));
            assert!(current_session_attribution().is_none());
            release_send.send(()).unwrap();
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(work, observer)
        })
        .await
        .unwrap();
        assert!(registry.since(registry.mark()).is_none());
        assert!(current_session_attribution().is_none());
    }
    #[test]
    fn finished_activity_wins_over_open_and_close_is_idempotent() {
        let registry = SessionActivityRegistry::default();
        let mut first = registry.open_span(
            SessionAttribution {
                session_id: Some("first".into()),
                tool: None,
                job: None,
            },
            None,
        );
        let mark = registry.mark();
        let _second = registry.open_span(
            SessionAttribution {
                session_id: Some("second".into()),
                tool: None,
                job: None,
            },
            None,
        );
        first.close();
        assert_eq!(
            registry.since(mark).unwrap().session_id.as_deref(),
            Some("first")
        );
        let after = registry.mark();
        first.close();
        assert_eq!(registry.mark(), after);
        assert_eq!(
            registry.since(after).unwrap().session_id.as_deref(),
            Some("second")
        );
    }
    #[test]
    fn settled_turn_closes_tool_spans() {
        let registry = SessionActivityRegistry::default();
        let mut tools = ToolAttributionSpans::new("session".into(), registry.clone());
        tools.observe(
            &serde_json::json!({"type":"tool_execution_start","toolCallId":"id","toolName":"bash"}),
        );
        assert_eq!(
            registry.since(registry.mark()).unwrap().tool.as_deref(),
            Some("bash")
        );
        tools.observe(&serde_json::json!({"type":"agent_settled"}));
        assert!(registry.since(registry.mark()).is_none());
    }
    #[test]
    fn span_inherits_only_session_id() {
        let registry = SessionActivityRegistry::default();
        let ambient = SessionAttribution {
            session_id: Some("ambient".into()),
            tool: Some("old".into()),
            job: Some("fanout".into()),
        };
        let _span = registry.open_span(SessionAttribution::default(), Some(&ambient));
        assert_eq!(
            registry.since(0).unwrap(),
            SessionAttribution {
                session_id: ambient.session_id,
                tool: None,
                job: None
            }
        );
    }
    #[test]
    fn describe_names_session_tool_and_job() {
        assert_eq!(describe_attribution(None), "no attributed session");
        assert_eq!(
            describe_attribution(Some(&SessionAttribution::default())),
            "no attributed session"
        );
        assert_eq!(
            describe_attribution(Some(&SessionAttribution {
                session_id: Some("s".into()),
                tool: Some("bash".into()),
                job: Some("fanout".into())
            })),
            "sessionId=s tool=bash job=fanout"
        );
        assert_eq!(
            describe_attribution(Some(&SessionAttribution {
                session_id: None,
                tool: None,
                job: Some("hook:x".into())
            })),
            "sessionId=unknown job=hook:x"
        );
    }
    #[test]
    fn activity_start_listener_fires_synchronously_until_unsubscribed() {
        let registry = SessionActivityRegistry::default();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        let unsubscribe = on_session_activity_start(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let before = hits.load(std::sync::atomic::Ordering::SeqCst);
        let span = registry.open_job_span("fanout");
        assert!(hits.load(std::sync::atomic::Ordering::SeqCst) > before);
        assert_eq!(
            registry.since(0).and_then(|a| a.job).as_deref(),
            Some("fanout")
        );
        drop(span);
        unsubscribe();
        let after = hits.load(std::sync::atomic::Ordering::SeqCst);
        let _again = registry.open_job_span("fanout");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), after);
    }
}
