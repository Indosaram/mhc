//! Projects the host's registry entries into the views `derive_session_status` reads (senpi
//! `session-status-source.ts`). An in-process entry is read from its live `AgentSession`; a worker-isolate entry from
//! the last state snapshot its worker published. Identity is read from the runtime first, because a
//! `new_session`/`switch_session` moves the durable id and file before the registry's own metadata sync runs.
//!
//! ADAPTATION: senpi reads the registry synchronously; the Rust registries sit behind async locks, so the host
//! pushes each entry's registry half here whenever it changes ([`HostStatusSource::upsert`]) and the runtime half
//! is read live, only while some subscription exists.
use crate::session_status::{SessionStatusEntryView, SessionStatusRuntimeView, SessionView};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Where an entry's runtime half is read from.
#[derive(Clone)]
pub enum StatusRuntime {
    None,
    Session(Arc<maho_core::agent_session_runtime::AgentSessionRuntime>),
    Worker(Arc<crate::session_worker_client::SessionWorkerClient>),
}

#[derive(Clone)]
struct SourceEntry {
    entry: SessionStatusEntryView,
    runtime: StatusRuntime,
}

/// The host's sessions as the status stream sees them (senpi `registryStatusSource`).
#[derive(Default)]
pub struct HostStatusSource {
    entries: BTreeMap<String, SourceEntry>,
}

impl HostStatusSource {
    pub fn upsert(
        &mut self,
        session_id: &str,
        entry: SessionStatusEntryView,
        runtime: StatusRuntime,
    ) {
        self.entries
            .insert(session_id.to_owned(), SourceEntry { entry, runtime });
    }

    pub fn remove(&mut self, session_id: &str) {
        self.entries.remove(session_id);
    }
}

/// The host's status stream together with the sessions it reads (senpi `SessionEventWriter.status` + its source).
#[derive(Default)]
pub struct SessionStatusHub {
    pub stream: crate::session_status_stream::SessionStatusStream,
    pub source: HostStatusSource,
}

/// What a live in-process session contributes (senpi `sessionRuntimeView`).
pub fn session_runtime_view(
    session: &maho_core::agent_session::AgentSession,
) -> SessionStatusRuntimeView {
    let model = session.model();
    SessionStatusRuntimeView {
        is_streaming: session.is_streaming(),
        is_compacting: session.is_compacting(),
        model: Some((model.provider.clone(), model.id.clone())),
        thinking_level: Some(session.thinking_level().as_str().to_owned()),
        steering: session.get_steering_messages().len(),
        follow_up: session.get_follow_up_messages().len(),
        // The host's pending question frames are counted by the fanout (`SessionStatusDelivery`).
        pending_questions: 0,
    }
}

/// What a worker's last published state contributes (senpi `runtimeView` over `worker.snapshot.state`).
pub fn worker_runtime_view(state: &Value) -> SessionStatusRuntimeView {
    let count = |key: &str| state[key].as_array().map_or(0, Vec::len);
    SessionStatusRuntimeView {
        is_streaming: state["isStreaming"] == true,
        is_compacting: state["isCompacting"] == true,
        model: state["model"]["provider"]
            .as_str()
            .zip(state["model"]["id"].as_str())
            .map(|(provider, id)| (provider.to_owned(), id.to_owned())),
        thinking_level: state["thinkingLevel"].as_str().map(str::to_owned),
        steering: count("steering"),
        follow_up: count("followUp"),
        pending_questions: count("pendingQuestions"),
    }
}

/// The registry fields of one entry [`entry_view`] projects (senpi `RpcSessionEntry`).
pub struct RegistryEntryFields<'a> {
    pub state: crate::session_registry::RpcSessionState,
    pub kind: maho_ext_api::SessionKind,
    pub cwd: &'a str,
    pub attachments: i64,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub context: &'a BTreeMap<String, String>,
    pub overlay: Option<&'a crate::session_context_patch::SessionContextOverlay>,
}

/// The registry half of an entry (senpi `entryView`): the published context is the open-time context overlaid with
/// every patch, and `open_context` is the frozen one `childrenOf` matching reads.
pub fn entry_view(fields: RegistryEntryFields<'_>) -> SessionStatusEntryView {
    let RegistryEntryFields {
        state,
        kind,
        cwd,
        attachments,
        durable_session_id,
        session_path,
        context,
        overlay,
    } = fields;
    let open_context: Map<String, Value> = context
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    SessionStatusEntryView {
        state,
        kind: if kind == maho_ext_api::SessionKind::Worker {
            "worker"
        } else {
            "interactive"
        },
        cwd: cwd.to_owned(),
        attachments,
        durable_session_id,
        session_path,
        name: None,
        context: crate::session_context_patch::published_session_context(context, overlay),
        open_context: Some(open_context),
    }
}

impl crate::session_status_stream::SessionStatusSource for HostStatusSource {
    fn session_ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    fn view(&self, session_id: &str) -> Option<SessionView> {
        let source = self.entries.get(session_id)?;
        let mut entry = source.entry.clone();
        let runtime = match &source.runtime {
            StatusRuntime::None => None,
            StatusRuntime::Session(runtime) => {
                let session = runtime.session();
                entry.durable_session_id = Some(session.session_id());
                if let Some(path) = session.session_file() {
                    entry.session_path = Some(path);
                }
                entry.name = session.session_name();
                Some(session_runtime_view(session))
            }
            StatusRuntime::Worker(worker) => worker.snapshot().map(|snapshot| {
                let state = &snapshot.state;
                if let Some(id) = state["sessionId"].as_str() {
                    entry.durable_session_id = Some(id.to_owned());
                }
                if let Some(path) = state["sessionFile"].as_str() {
                    entry.session_path = Some(path.to_owned());
                }
                entry.name = state["sessionName"].as_str().map(str::to_owned);
                worker_runtime_view(state)
            }),
        };
        Some(SessionView { entry, runtime })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_status_stream::SessionStatusSource;
    use serde_json::json;

    #[test]
    fn worker_state_projects_queues_and_model() {
        let view = worker_runtime_view(&json!({
            "isStreaming": true, "isCompacting": false,
            "model": {"provider": "p", "id": "m"}, "thinkingLevel": "high",
            "steering": ["a"], "followUp": ["b", "c"], "pendingQuestions": [{}],
        }));
        assert_eq!(view.model, Some(("p".into(), "m".into())));
        assert_eq!(
            (view.steering, view.follow_up, view.pending_questions),
            (1, 2, 1)
        );
        assert!(view.is_streaming && !view.is_compacting);
    }

    #[test]
    fn entries_publish_the_patched_context_and_keep_the_frozen_one() {
        let context = BTreeMap::from([("parent_session_id".to_owned(), "p".to_owned())]);
        let overlay = json!({"parent_session_id": "moved", "label": "x"});
        let mut source = HostStatusSource::default();
        source.upsert(
            "s1",
            entry_view(RegistryEntryFields {
                state: crate::session_registry::RpcSessionState::Opening,
                kind: maho_ext_api::SessionKind::Worker,
                cwd: "/w",
                attachments: 0,
                durable_session_id: None,
                session_path: None,
                context: &context,
                overlay: overlay.as_object(),
            }),
            StatusRuntime::None,
        );
        let view = source.view("s1").unwrap();
        assert_eq!((view.entry.kind, view.runtime), ("worker", None));
        assert_eq!(view.entry.context["parent_session_id"], "moved");
        assert_eq!(view.entry.open_context.unwrap()["parent_session_id"], "p");
        source.remove("s1");
        assert!(source.session_ids().is_empty());
    }
}
