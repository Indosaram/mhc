//! Connection-scoped security notice replay and worker grant custody.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub type Cancel = Arc<dyn Fn() + Send + Sync>;

struct PresentationState {
    durable_id: Option<String>,
    snapshot: Value,
    replayed: BTreeMap<String, String>,
    grants: BTreeMap<String, (Option<String>, Cancel)>,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self {
            durable_id: None,
            snapshot: json!({"generation": 0, "notices": []}),
            replayed: BTreeMap::new(),
            grants: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
pub struct RpcSecurityPresentations {
    sessions: BTreeMap<String, PresentationState>,
    attachments: BTreeMap<String, BTreeSet<String>>,
}

impl RpcSecurityPresentations {
    pub fn bind(&mut self, session: &str, durable_id: &str, snapshot: Value) {
        let mut old = self.sessions.remove(session).unwrap_or_default();
        for (_, (_, cancel)) in std::mem::take(&mut old.grants) {
            cancel();
        }
        if old.durable_id.as_deref() != Some(durable_id) {
            old.replayed.clear();
        }
        old.durable_id = Some(durable_id.into());
        old.snapshot = snapshot;
        self.sessions.insert(session.into(), old);
    }

    pub fn bind_and_replay(
        &mut self,
        session: &str,
        durable_id: &str,
        snapshot: Value,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> Result<(), String> {
        self.bind(session, durable_id, snapshot.clone());
        self.accept_snapshot(session, snapshot, fanout)
    }

    pub fn accept_snapshot(
        &mut self,
        session: &str,
        snapshot: Value,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> Result<(), String> {
        self.sessions.entry(session.into()).or_default().snapshot = snapshot;
        for id in fanout.attached_connections(session) {
            self.replay(session, &id, fanout)?;
        }
        Ok(())
    }

    pub fn attach(
        &mut self,
        id: &str,
        session: &str,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> Result<(), String> {
        if !self
            .attachments
            .entry(id.into())
            .or_default()
            .insert(session.into())
        {
            return Ok(());
        }
        self.replay(session, id, fanout)
    }

    pub fn detach(&mut self, id: &str, session: &str) {
        if let Some(attached) = self.attachments.get_mut(id) {
            attached.remove(session);
            if attached.is_empty() {
                self.attachments.remove(id);
            }
        }
        if let Some(state) = self.sessions.get_mut(session) {
            state.replayed.remove(id);
            Self::cancel_for(state, id);
        }
    }

    pub fn drop_connection(&mut self, id: &str) {
        let sessions = self.attachments.get(id).cloned().unwrap_or_default();
        for session in sessions {
            self.detach(id, &session);
        }
    }

    pub fn capabilities_changed(
        &mut self,
        id: &str,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) {
        if fanout.connection_has(id, "grant_select") {
            return;
        }
        let sessions = self.attachments.get(id).cloned().unwrap_or_default();
        for session in sessions {
            if let Some(state) = self.sessions.get_mut(&session) {
                Self::cancel_for(state, id);
            }
        }
    }

    pub fn route_worker_grant(
        &mut self,
        session: &str,
        request: &str,
        preferred: Option<&str>,
        fallback: &[String],
        cancel: Cancel,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> (bool, Option<String>) {
        let attached = fanout.attached_connections(session);
        let connection = preferred
            .filter(|id| {
                attached.iter().any(|attached| attached == id)
                    && fanout.connection_has(id, "grant_select")
            })
            .map(str::to_owned)
            .or_else(|| {
                attached
                    .into_iter()
                    .find(|id| fanout.connection_has(id, "grant_select"))
            });
        let accepted = connection.is_some()
            || (fanout.is_empty()
                && fallback
                    .iter()
                    .any(|capability| capability == "grant_select"));
        if !accepted {
            cancel();
            return (false, None);
        }
        self.sessions
            .entry(session.into())
            .or_default()
            .grants
            .insert(request.into(), (connection.clone(), cancel));
        (true, connection)
    }

    pub fn accept_worker_response(
        &mut self,
        session: &str,
        request: &str,
        connection: Option<&str>,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> Option<bool> {
        let state = self.sessions.get_mut(session)?;
        let pending = state.grants.get(request)?;
        if pending.0.as_deref() != connection
            || connection.is_some_and(|id| !fanout.connection_has(id, "grant_select"))
        {
            return Some(false);
        }
        state.grants.remove(request);
        Some(true)
    }

    pub fn settle_grant(&mut self, session: &str, request: &str) {
        if let Some(state) = self.sessions.get_mut(session) {
            state.grants.remove(request);
        }
    }

    pub fn drop_session(&mut self, session: &str) {
        if let Some(state) = self.sessions.remove(session) {
            for (_, (_, cancel)) in state.grants {
                cancel();
            }
        }
        self.attachments.retain(|_, sessions| {
            sessions.remove(session);
            !sessions.is_empty()
        });
    }

    pub fn snapshot(&self, session: &str) -> Option<&Value> {
        self.sessions.get(session).map(|s| &s.snapshot)
    }

    pub fn has_session(&self, session: &str) -> bool {
        self.sessions.contains_key(session)
    }

    pub fn pending_grant_count(&self, session: &str) -> usize {
        self.sessions.get(session).map_or(0, |s| s.grants.len())
    }

    pub fn is_replayed(&self, session: &str, id: &str) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|s| s.replayed.contains_key(id))
    }

    fn replay(
        &mut self,
        session: &str,
        id: &str,
        fanout: &crate::session_event_fanout::SessionEventFanout,
    ) -> Result<(), String> {
        if !fanout.is_registered(id) {
            return Ok(());
        }
        let Some(state) = self.sessions.get_mut(session) else {
            return Ok(());
        };
        let revision = state.snapshot.to_string();
        if state.replayed.get(id) == Some(&revision) {
            return Ok(());
        }
        if let Some(notices) = state.snapshot.get("notices").and_then(Value::as_array) {
            for notice in notices {
                let Some(message) = notice.get("message").and_then(Value::as_str) else {
                    continue;
                };
                fanout.enqueue_control(
                    id,
                    &json!({
                        "type": "extension_ui_request",
                        "id": uuid::Uuid::new_v4().to_string(),
                        "sessionId": session,
                        "method": "notify",
                        "notifyType": "warning",
                        "message": message,
                    }),
                )?;
            }
        }
        state.replayed.insert(id.into(), revision);
        Ok(())
    }

    fn cancel_for(state: &mut PresentationState, id: &str) {
        let keys = state
            .grants
            .iter()
            .filter(|(_, pending)| pending.0.as_deref() == Some(id))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if let Some((_, cancel)) = state.grants.remove(&key) {
                cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn make_test_actor(
        writer: tokio::io::DuplexStream,
    ) -> crate::socket_event_fanout::SocketEventSinkActor {
        crate::socket_event_fanout::SocketEventSinkActor::new(
            writer,
            4096,
            30000,
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::loop_blocked_time::LoopBlockedTime::default(),
            )),
            |_| {},
        )
    }

    #[test]
    fn absent_capable_connection_cancels_grant() {
        let fanout = crate::session_event_fanout::SessionEventFanout::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = cancelled.clone();
        let mut presentations = RpcSecurityPresentations::default();
        assert_eq!(
            presentations.route_worker_grant(
                "s",
                "r",
                None,
                &[],
                Arc::new(move || {
                    observed.store(true, Ordering::SeqCst);
                }),
                &fanout
            ),
            (false, None)
        );
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn route_worker_grant_selects_capable_attached_connection() {
        let mut fanout = crate::session_event_fanout::SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);
        let (w2, _r2) = tokio::io::duplex(1024);

        fanout.register("c-normal", make_test_actor(w1));
        fanout
            .set_capabilities("c-normal", &["question".into()])
            .unwrap();
        fanout.attach("c-normal", "session-a", 0.0).unwrap();

        fanout.register("c-capable", make_test_actor(w2));
        fanout
            .set_capabilities("c-capable", &["grant_select".into()])
            .unwrap();
        fanout.attach("c-capable", "session-a", 0.0).unwrap();

        let mut presentations = RpcSecurityPresentations::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (accepted, conn) = presentations.route_worker_grant(
            "session-a",
            "req-1",
            None,
            &[],
            { let cancelled = cancelled.clone(); Arc::new(move || { cancelled.store(true, Ordering::SeqCst); }) },
            &fanout,
        );
        assert!(accepted);
        assert_eq!(conn, Some("c-capable".into()));
        assert!(!cancelled.load(Ordering::SeqCst));
        assert_eq!(presentations.pending_grant_count("session-a"), 1);

        // Verification of response authorization
        assert_eq!(
            presentations.accept_worker_response("session-a", "req-1", Some("c-normal"), &fanout),
            Some(false)
        );
        assert_eq!(
            presentations.accept_worker_response("session-a", "req-1", Some("c-capable"), &fanout),
            Some(true)
        );
        assert_eq!(presentations.pending_grant_count("session-a"), 0);
    }

    #[tokio::test]
    async fn route_worker_grant_prefers_specified_capable_connection() {
        let mut fanout = crate::session_event_fanout::SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);
        let (w2, _r2) = tokio::io::duplex(1024);

        fanout.register("c1", make_test_actor(w1));
        fanout
            .set_capabilities("c1", &["grant_select".into()])
            .unwrap();
        fanout.attach("c1", "s", 0.0).unwrap();

        fanout.register("c2", make_test_actor(w2));
        fanout
            .set_capabilities("c2", &["grant_select".into()])
            .unwrap();
        fanout.attach("c2", "s", 0.0).unwrap();

        let mut presentations = RpcSecurityPresentations::default();
        let cancelled = Arc::new(AtomicBool::new(false));

        // When preferred is c2, it picks c2 even though c1 is first in attach order
        let (accepted, conn) = presentations.route_worker_grant(
            "s",
            "req-2",
            Some("c2"),
            &[],
            { let cancelled = cancelled.clone(); Arc::new(move || { cancelled.store(true, Ordering::SeqCst); }) },
            &fanout,
        );
        assert!(accepted);
        assert_eq!(conn, Some("c2".into()));
    }

    #[test]
    fn route_worker_grant_falls_back_when_fanout_empty() {
        let fanout = crate::session_event_fanout::SessionEventFanout::default();
        let mut presentations = RpcSecurityPresentations::default();
        let cancelled = Arc::new(AtomicBool::new(false));

        let (accepted, conn) = presentations.route_worker_grant(
            "s",
            "req-3",
            None,
            &["grant_select".into()],
            { let cancelled = cancelled.clone(); Arc::new(move || { cancelled.store(true, Ordering::SeqCst); }) },
            &fanout,
        );
        assert!(accepted);
        assert_eq!(conn, None);
        assert!(!cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn capabilities_changed_cancels_grant_on_capability_loss() {
        let mut fanout = crate::session_event_fanout::SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);

        fanout.register("client", make_test_actor(w1));
        fanout
            .set_capabilities("client", &["grant_select".into()])
            .unwrap();
        fanout.attach("client", "session-cap", 0.0).unwrap();

        let mut presentations = RpcSecurityPresentations::default();
        presentations
            .attach("client", "session-cap", &fanout)
            .unwrap();

        let cancelled = Arc::new(AtomicBool::new(false));
        let obs = cancelled.clone();
        let (accepted, _) = presentations.route_worker_grant(
            "session-cap",
            "req-cap",
            None,
            &[],
            Arc::new(move || {
                obs.store(true, Ordering::SeqCst);
            }),
            &fanout,
        );
        assert!(accepted);

        // Capability removed from fanout
        fanout.set_capabilities("client", &[]).unwrap();
        presentations.capabilities_changed("client", &fanout);

        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(presentations.pending_grant_count("session-cap"), 0);
    }

    #[tokio::test]
    async fn detach_and_drop_session_lifecycle() {
        let mut fanout = crate::session_event_fanout::SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);

        fanout.register("c1", make_test_actor(w1));
        fanout
            .set_capabilities("c1", &["grant_select".into()])
            .unwrap();
        fanout.attach("c1", "s-life", 0.0).unwrap();

        let mut presentations = RpcSecurityPresentations::default();
        presentations.attach("c1", "s-life", &fanout).unwrap();

        let cancelled = Arc::new(AtomicBool::new(false));
        let obs = cancelled.clone();
        presentations.route_worker_grant(
            "s-life",
            "req-life",
            None,
            &[],
            Arc::new(move || {
                obs.store(true, Ordering::SeqCst);
            }),
            &fanout,
        );

        // Detaching cancels grant for c1
        presentations.detach("c1", "s-life");
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(presentations.pending_grant_count("s-life"), 0);

        // Drop session cleans up
        presentations.drop_session("s-life");
        assert!(!presentations.has_session("s-life"));
    }

    #[test]
    fn bind_preserves_replayed_only_when_durable_id_matches() {
        let mut presentations = RpcSecurityPresentations::default();
        presentations.bind("s", "durable-1", json!({"generation": 0, "notices": []}));

        // Simulate replaying
        if let Some(state) = presentations.sessions.get_mut("s") {
            state.replayed.insert("c1".into(), "rev1".into());
        }
        assert!(presentations.is_replayed("s", "c1"));

        // Re-binding with SAME durable ID preserves replayed
        presentations.bind("s", "durable-1", json!({"generation": 1, "notices": []}));
        assert!(presentations.is_replayed("s", "c1"));

        // Re-binding with DIFFERENT durable ID clears replayed
        presentations.bind("s", "durable-2", json!({"generation": 0, "notices": []}));
        assert!(!presentations.is_replayed("s", "c1"));
    }
}
