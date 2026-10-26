use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub const RENDERED_COMPONENT_RECORD: &str = "__senpiRenderedComponent";
struct Connection {
    id: String,
    capabilities: Vec<String>,
    registered_capabilities: bool,
    sessions: BTreeSet<String>,
}
#[derive(Default)]
pub struct ConnectionTargets {
    connections: Vec<Connection>,
}
#[derive(Default)]
pub struct SessionEventFanout {
    grants: BTreeMap<(String, String), String>,
    attach_order: crate::session_attach_order::SessionAttachOrder,
    security: BTreeMap<String, Value>,
    security_replayed: BTreeMap<(String, String), String>,
    pub targets: ConnectionTargets,
    pub snapshots: SessionSnapshots,
    actors: BTreeMap<String, crate::socket_event_fanout::SocketEventSinkActor>,
    worker_sessions: BTreeSet<String>,
    /// What a shed `event_resync` peer's snapshot reads; shared because the actor reads it off the fanout lock.
    resync: std::sync::Arc<std::sync::Mutex<ResyncView>>,
}
/// Per-connection attachments and each session's latest in-flight message line, kept only while some connection
/// negotiated `event_resync` (senpi `resyncSnapshot` reads `connectionSessions` and `sessionSnapshots`).
#[derive(Default)]
struct ResyncView {
    attached: BTreeMap<String, BTreeSet<String>>,
    latest: BTreeMap<String, String>,
    recovering: BTreeSet<String>,
}
impl SessionEventFanout {
    pub fn grants_for_connection(&self, connection: &str) -> Vec<(String, String)> {
        self.grants
            .iter()
            .filter(|(_, owner)| owner.as_str() == connection)
            .map(|(key, _)| key.clone())
            .collect()
    }
    pub fn accept_grant_response(
        &mut self,
        session: &str,
        request: &str,
        connection: Option<&str>,
    ) -> bool {
        let key = (session.into(), request.into());
        let Some(owner) = self.grants.get(&key) else {
            return true;
        };
        if connection != Some(owner.as_str()) || !self.connection_has(owner, "grant_select") {
            return false;
        }
        self.grants.remove(&key);
        true
    }
    pub fn settle_grant(&mut self, session: &str, request: &str) {
        self.grants.remove(&(session.into(), request.into()));
    }
    pub fn attached_connections(&self, session: &str) -> Vec<String> {
        self.attach_order.connections(session)
    }
    pub fn connection_has(&self, id: &str, capability: &str) -> bool {
        self.targets
            .capabilities(id)
            .is_some_and(|caps| caps.iter().any(|cap| cap == capability))
    }
    pub fn is_empty(&self) -> bool {
        self.targets.connections.is_empty()
    }
    pub fn security_snapshot(&self, session: &str) -> Option<&Value> {
        self.security.get(session)
    }
    fn resync_view(&self) -> std::sync::MutexGuard<'_, ResyncView> {
        self.resync
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Detaches `id` from `session` (the router's `detachConnectionFromSession`).
    pub fn detach(&mut self, id: &str, session: &str) {
        self.attach_order.detached(session, id);
        self.security_replayed.remove(&(session.into(), id.into()));
        self.grants
            .retain(|(s, _), owner| !(s == session && owner == id));
        self.targets.detach(id, session);
        if let Some(sessions) = self.resync_view().attached.get_mut(id) {
            sessions.remove(session);
        }
    }
    fn enable_resync(&self, id: &str, placeholders: bool) {
        let Some(actor) = self.actors.get(id) else {
            return;
        };
        self.resync_view().recovering.insert(id.to_owned());
        let view = std::sync::Arc::clone(&self.resync);
        let connection = id.to_owned();
        actor.enable_recovery(Box::new(move |reason| {
            let attached = view
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .attached
                .get(&connection)
                .cloned()
                .unwrap_or_default();
            let view = std::sync::Arc::clone(&view);
            let connection = connection.clone();
            Box::new(move || {
                let view = view
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let current = view.attached.get(&connection).cloned().unwrap_or_default();
                attached
                    .union(&current)
                    .map(|session| {
                        // The serialized record, not the producer's object, fixes the recovery cut.
                        let message = view
                            .latest
                            .get(session)
                            .and_then(|line| serde_json::from_str::<Value>(line).ok())
                            .and_then(|record| record.get("message").cloned())
                            .unwrap_or(Value::Null);
                        let mut record = crate::socket_peer_recovery::RpcSessionResyncRequired {
                            session_id: session.clone(),
                            reason,
                            message,
                            closed: !current.contains(session),
                        }
                        .to_value();
                        if placeholders {
                            record =
                                crate::media_placeholders::omit_inline_media(&record).into_owned();
                        }
                        record
                    })
                    .collect()
            })
        }));
    }
    fn disable_resync(&self, id: &str) {
        self.resync_view().recovering.remove(id);
        if let Some(actor) = self.actors.get(id) {
            actor.disable_recovery();
        }
    }
    fn note_latest(&self, session: &str, wire: &Value, line: &str) {
        let mut view = self.resync_view();
        if view.recovering.is_empty() {
            return;
        }
        if wire["type"] == "message_end" {
            view.latest.remove(session);
        } else if !wire["message"].is_null() && self.snapshots.retains(session, wire) {
            view.latest.insert(session.to_owned(), line.to_owned());
        }
    }
}
impl SessionEventFanout {
    pub fn register(&mut self, id: &str, actor: crate::socket_event_fanout::SocketEventSinkActor) {
        self.targets.register(id);
        self.actors.insert(id.into(), actor);
    }
    pub fn unregister(&mut self, id: &str) {
        let sessions = self.attached_connections_for(id);
        for session in sessions {
            self.attach_order.detached(&session, id);
        }
        self.security_replayed.retain(|(_, conn), _| conn != id);
        self.grants.retain(|_, owner| owner != id);
        self.targets.unregister(id);
        self.actors.remove(id);
        let mut view = self.resync_view();
        view.attached.remove(id);
        view.recovering.remove(id);
    }
    /// Queues one untagged control record for a registered connection (senpi `enqueueControl`);
    /// `false` when the connection has no actor and the caller must use the stdio lane.
    pub fn enqueue_control(&self, id: &str, value: &Value) -> Result<bool, String> {
        let Some(actor) = self.actors.get(id) else {
            return Ok(false);
        };
        let line = crate::jsonl::serialize_json_line(value).map_err(|error| error.to_string())?;
        actor
            .enqueue(crate::socket_event_fanout::QueueEntry {
                line,
                key: None,
                demoted_line: None,
                on_written: None,
            })
            .map_err(|error| error.to_string())?;
        Ok(true)
    }
    pub fn broadcast_host_record(&mut self, record: &Value) -> Result<Option<Value>, String> {
        if self.actors.is_empty() {
            return Ok(Some(record.clone()));
        }
        let line = crate::jsonl::serialize_json_line(record).map_err(|error| error.to_string())?;
        let mut failed = Vec::new();
        for (id, actor) in &self.actors {
            if actor.failure().is_some()
                || actor
                    .enqueue(crate::socket_event_fanout::QueueEntry {
                        line: line.clone(),
                        key: None,
                        demoted_line: None,
                        on_written: None,
                    })
                    .is_err()
            {
                failed.push(id.clone());
            }
        }
        for id in failed {
            self.unregister(&id);
        }
        Ok(None)
    }
    fn attached_connections_for(&self, id: &str) -> Vec<String> {
        self.targets
            .connections
            .iter()
            .find(|connection| connection.id == id)
            .map(|connection| connection.sessions.iter().cloned().collect())
            .unwrap_or_default()
    }
    fn replay_security(&mut self, session: &str, id: &str) -> Result<(), String> {
        if !self.is_registered(id) {
            return Ok(());
        }
        let Some(snapshot) = self.security.get(session) else {
            return Ok(());
        };
        let revision = snapshot.to_string();
        let key = (session.into(), id.into());
        if self.security_replayed.get(&key) == Some(&revision) {
            return Ok(());
        }
        if let Some(notices) = snapshot.get("notices").and_then(Value::as_array) {
            for notice in notices {
                let Some(message) = notice.get("message").and_then(Value::as_str) else {
                    continue;
                };
                self.enqueue_control(
                    id,
                    &serde_json::json!({
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
        self.security_replayed.insert(key, revision);
        Ok(())
    }
    pub fn set_session_kind(&mut self, session: &str, kind: maho_ext_api::SessionKind) {
        if kind == maho_ext_api::SessionKind::Worker {
            self.worker_sessions.insert(session.into());
        } else {
            self.worker_sessions.remove(session);
        }
    }
    pub fn forget_session(&mut self, session: &str) {
        {
            let mut view = self.resync_view();
            view.latest.remove(session);
            for sessions in view.attached.values_mut() {
                sessions.remove(session);
            }
        }
        self.attach_order.forget_session(session);
        self.security.remove(session);
        self.security_replayed.retain(|(key, _), _| key != session);
        self.grants.retain(|(s, _), _| s != session);
        self.snapshots.forget(session);
        self.worker_sessions.remove(session);
        self.targets.forget_session(session);
    }
    /// Whether a live connection with a sink actor is registered under `id`.
    pub fn is_registered(&self, id: &str) -> bool {
        self.actors.contains_key(id)
    }
    pub fn set_capabilities(&mut self, id: &str, capabilities: &[String]) -> Result<(), String> {
        if capabilities
            .iter()
            .any(|capability| capability == crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY)
        {
            self.enable_resync(
                id,
                capabilities.iter().any(|capability| {
                    capability == crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY
                }),
            );
        } else {
            self.disable_resync(id);
        }
        if !self.targets.set_capabilities(id, capabilities) {
            return Ok(());
        }
        let placeholders = capabilities.iter().any(|capability| {
            capability == crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY
        });
        let sessions = self
            .targets
            .connections
            .iter()
            .find(|connection| connection.id == id)
            .map(|connection| connection.sessions.clone())
            .unwrap_or_default();
        if let Some(actor) = self.actors.get(id) {
            for session in sessions {
                for record in self
                    .snapshots
                    .snapshots
                    .get(&session)
                    .into_iter()
                    .flatten()
                    .filter(|record| record.rendered)
                {
                    let line = if placeholders {
                        match &record.source {
                            Some(source) => crate::jsonl::serialize_json_line(
                                &crate::media_placeholders::omit_inline_media(source),
                            )
                            .map_err(|error| error.to_string())?,
                            None => record.line.clone(),
                        }
                    } else {
                        record.line.clone()
                    };
                    actor
                        .enqueue(crate::socket_event_fanout::QueueEntry {
                            line,
                            key: None,
                            demoted_line: None,
                            on_written: None,
                        })
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }
    pub fn attach(&mut self, id: &str, session: &str, now_ms: f64) -> Result<(), String> {
        if !self.targets.attach(id, session) {
            return Ok(());
        }
        self.attach_order.attached(session, id);
        self.replay_security(session, id)?;
        self.resync_view()
            .attached
            .entry(id.to_owned())
            .or_default()
            .insert(session.to_owned());
        let capabilities = self.targets.capabilities(id).unwrap_or_default();
        let rendered = capabilities
            .iter()
            .any(|value| value == crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY);
        let placeholders = capabilities
            .iter()
            .any(|value| value == crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY);
        let lines = self
            .snapshots
            .replay(session, rendered, placeholders, now_ms)
            .map_err(|error| error.to_string())?;
        if let Some(actor) = self.actors.get(id) {
            for line in lines {
                actor
                    .enqueue(crate::socket_event_fanout::QueueEntry {
                        line,
                        key: None,
                        demoted_line: None,
                        on_written: None,
                    })
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
    pub fn deliver(
        &mut self,
        session: &str,
        target: Option<&str>,
        targeted: bool,
        value: &Value,
    ) -> Result<Vec<Value>, String> {
        self.deliver_owned(session, target, targeted, value.clone())
    }
    /// [`Self::deliver`] for a record the caller owns: the wire record is edited in place instead
    /// of deep-copying the cumulative `message_update` snapshot on every delta.
    pub fn deliver_owned(
        &mut self,
        session: &str,
        target: Option<&str>,
        targeted: bool,
        mut wire: Value,
    ) -> Result<Vec<Value>, String> {
        let failed = self
            .actors
            .iter()
            .filter(|(_, actor)| actor.failure().is_some())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in failed {
            self.unregister(&id);
        }
        if matches!(
            wire["type"].as_str(),
            Some("security_notices" | "security_notice_snapshot")
        ) {
            self.security.insert(
                session.into(),
                wire.get("snapshot")
                    .cloned()
                    .unwrap_or_else(|| wire.clone()),
            );
            for id in self.attached_connections(session) {
                self.replay_security(session, &id)?;
            }
            return Ok(Vec::new());
        }
        let grant_connection = if wire["grantSelect"] == true {
            let attached = self.attached_connections(session);
            let selected = target
                .filter(|id| {
                    attached.iter().any(|attached| attached == id)
                        && self.connection_has(id, "grant_select")
                })
                .map(str::to_owned)
                .or_else(|| {
                    attached
                        .into_iter()
                        .find(|id| self.connection_has(id, "grant_select"))
                });
            if let Some(id) = &selected {
                if let Some(request) = wire["id"].as_str() {
                    self.grants
                        .insert((session.into(), request.into()), id.clone());
                }
            } else if !self.is_empty() {
                return Ok(Vec::new());
            }
            selected
        } else {
            None
        };
        let target = grant_connection.as_deref().or(target);
        let targeted = targeted || grant_connection.is_some();
        let rendered = wire[RENDERED_COMPONENT_RECORD] == true;
        if let Some(object) = wire.as_object_mut() {
            object.remove(RENDERED_COMPONENT_RECORD);
            object.insert("sessionId".into(), session.into());
        }
        let line = crate::jsonl::serialize_json_line(&wire).map_err(|error| error.to_string())?;
        let terminal = matches!(
            wire["type"].as_str(),
            Some("session_closed" | "session_parked")
        );
        if terminal {
            self.snapshots.forget(session);
            self.resync_view().latest.remove(session);
        } else if !targeted {
            let tagged = if rendered {
                let mut tagged = wire.clone();
                tagged[RENDERED_COMPONENT_RECORD] = true.into();
                std::borrow::Cow::Owned(tagged)
            } else {
                std::borrow::Cow::Borrowed(&wire)
            };
            // `omit_inline_media` returns any other record as-is, so its placeholder replay is
            // `line` itself; keep a source copy only for a record the snapshot list will retain.
            let source = (crate::media_placeholders::carries_inline_media(&wire)
                && self.snapshots.retains(session, &wire))
            .then(|| wire.clone());
            self.note_latest(session, &wire, &line);
            self.snapshots
                .remember(session, &tagged, line.clone(), None, source);
        }
        let mut failed = Vec::new();
        let mut stdio = Vec::new();
        let kind = if terminal {
            if self.worker_sessions.contains(session) {
                None
            } else {
                Some("session_closed")
            }
        } else {
            wire["type"].as_str()
        };
        let targets = self
            .targets
            .targets(session, target, targeted, rendered, kind);
        let last = targets.len();
        // PERF-DIVERGENCE(senpi socket-event-fanout): upstream stringifies the scrubbed/demoted record per connection; we serialize each variant once per placeholders flag and share it across targets. Bytes on the wire are identical.
        let mut variant_lines: [Option<(String, Option<String>)>; 2] = [None, None];
        for (index, id) in targets.into_iter().enumerate() {
            // The last target takes the record itself; earlier stdio targets get a copy.
            let Some(id) = id else {
                stdio.push(if index + 1 == last {
                    std::mem::take(&mut wire)
                } else {
                    wire.clone()
                });
                continue;
            };
            let Some(actor) = self.actors.get(&id) else {
                continue;
            };
            let placeholders = self
                .targets
                .capabilities(&id)
                .unwrap_or_default()
                .iter()
                .any(|capability| {
                    capability == crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY
                });
            let slot = &mut variant_lines[usize::from(placeholders)];
            if slot.is_none() {
                let selected = if placeholders {
                    crate::media_placeholders::omit_inline_media(&wire)
                } else {
                    std::borrow::Cow::Borrowed(&wire)
                };
                // A borrowed `selected` is `wire` itself, whose bytes are already `line`.
                let line = match &selected {
                    std::borrow::Cow::Owned(scrubbed) => {
                        crate::jsonl::serialize_json_line(scrubbed)
                            .map_err(|error| error.to_string())?
                    }
                    std::borrow::Cow::Borrowed(_) => line.clone(),
                };
                let keyed = selected["type"] == "message_update"
                    && !selected["message"].is_null()
                    && matches!(
                        selected["assistantMessageEvent"]["type"].as_str(),
                        Some("text_delta" | "thinking_delta" | "toolcall_delta")
                    )
                    && selected["assistantMessageEvent"]["contentIndex"].is_number()
                    && selected["assistantMessageEvent"]["delta"].is_string();
                let demoted_line = if keyed {
                    Some(
                        crate::jsonl::serialize_json_line(&demote_snapshot(&selected))
                            .map_err(|error| error.to_string())?,
                    )
                } else {
                    None
                };
                *slot = Some((line, demoted_line));
            }
            let (line, demoted_line) = slot.clone().unwrap_or_default();
            let keyed = demoted_line.is_some();
            if actor
                .enqueue(crate::socket_event_fanout::QueueEntry {
                    line,
                    key: keyed.then(|| "message".into()),
                    demoted_line,
                    on_written: None,
                })
                .is_err()
            {
                failed.push(id);
            }
        }
        for id in failed {
            self.unregister(&id);
        }
        Ok(stdio)
    }
    pub async fn flush_connection(&mut self, id: &str) -> Result<(), String> {
        if let Some(actor) = self.actors.get_mut(id) {
            actor.flush().await?;
        }
        Ok(())
    }
}
impl ConnectionTargets {
    pub fn register(&mut self, id: &str) {
        if let Some(connection) = self
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
        {
            *connection = Connection {
                id: id.into(),
                capabilities: vec![],
                registered_capabilities: false,
                sessions: BTreeSet::new(),
            };
        } else {
            self.connections.push(Connection {
                id: id.into(),
                capabilities: vec![],
                registered_capabilities: false,
                sessions: BTreeSet::new(),
            });
        }
    }
    pub fn unregister(&mut self, id: &str) {
        self.connections.retain(|connection| connection.id != id);
    }
    /// Drops a finished handle from every connection's attachment set.
    pub fn forget_session(&mut self, session: &str) {
        for connection in &mut self.connections {
            connection.sessions.remove(session);
        }
    }
    pub fn attach(&mut self, id: &str, session: &str) -> bool {
        self.connections
            .iter_mut()
            .find(|connection| connection.id == id)
            .is_some_and(|connection| connection.sessions.insert(session.into()))
    }
    pub fn detach(&mut self, id: &str, session: &str) {
        if let Some(connection) = self
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
        {
            connection.sessions.remove(session);
        }
    }
    pub fn set_capabilities(&mut self, id: &str, capabilities: &[String]) -> bool {
        let Some(connection) = self
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
        else {
            return false;
        };
        let was = connection.capabilities.iter().any(|capability| {
            capability == crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY
        });
        connection.capabilities = capabilities.iter().fold(vec![], |mut unique, capability| {
            if !unique.contains(capability) {
                unique.push(capability.clone());
            }
            unique
        });
        connection.registered_capabilities = true;
        !was && connection.capabilities.iter().any(|capability| {
            capability == crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY
        })
    }
    pub fn capabilities(&self, id: &str) -> Option<&[String]> {
        self.connections
            .iter()
            .find(|connection| connection.id == id && connection.registered_capabilities)
            .map(|connection| connection.capabilities.as_slice())
    }
    pub fn clear_capabilities(&mut self, id: &str) {
        if let Some(connection) = self
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
        {
            connection.capabilities.clear();
            connection.registered_capabilities = false;
        }
    }
    pub fn targets(
        &self,
        session: &str,
        target: Option<&str>,
        targeted: bool,
        rendered: bool,
        record_type: Option<&str>,
    ) -> Vec<Option<String>> {
        if targeted {
            return vec![target.map(str::to_owned)];
        }
        if self.connections.is_empty() {
            return vec![None];
        }
        let broadcast = record_type.is_some_and(|kind| {
            matches!(
                kind,
                "agent_start"
                    | "agent_settled"
                    | "agent_idle"
                    | "session_lifecycle"
                    | "session_opened"
                    | "session_closed"
            )
        });
        self.connections
            .iter()
            .filter(|connection| {
                broadcast
                    || (connection.sessions.contains(session)
                        && (!rendered
                            || connection.capabilities.iter().any(|capability| {
                                capability
                                    == crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY
                            })))
            })
            .map(|connection| Some(connection.id.clone()))
            .collect()
    }

    /// Every connection attached to `session` (senpi `SessionEventFanout.attachedConnections`).
    pub fn attached_connections(&self, session: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|connection| connection.sessions.contains(session))
            .map(|connection| connection.id.clone())
            .collect()
    }

    /// Whether `connection` registered `capability` in its own handshake (senpi `connectionHas`).
    pub fn connection_has(&self, connection: &str, capability: &str) -> bool {
        self.connections.iter().any(|candidate| {
            candidate.id == connection
                && candidate
                    .capabilities
                    .iter()
                    .any(|name| name == capability)
        })
    }

    /// Whether no connection is registered at all (senpi `SessionEventFanout.isEmpty`).
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}
struct SnapshotRecord {
    line: String,
    placeholder_line: Option<String>,
    source: Option<Value>,
    rendered: bool,
}
/// `record` with `message` and `assistantMessageEvent.partial` nulled, built by copying the small
/// entries instead of deep-cloning the cumulative snapshots first. Key order matches assigning
/// both fields on a clone: existing keys keep their slot, a missing `partial` is appended.
fn demote_snapshot(record: &Value) -> Value {
    let Some(object) = record.as_object() else {
        return record.clone();
    };
    let demote_event = |event: &Value| match event.as_object() {
        Some(event) => {
            let mut event = event
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if key == "partial" {
                            Value::Null
                        } else {
                            value.clone()
                        },
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            event.entry("partial").or_insert(Value::Null);
            Value::Object(event)
        }
        None => {
            let mut event = event.clone();
            event["partial"] = Value::Null;
            event
        }
    };
    let mut demoted = object
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                match key.as_str() {
                    "message" => Value::Null,
                    "assistantMessageEvent" => demote_event(value),
                    _ => value.clone(),
                },
            )
        })
        .collect::<serde_json::Map<_, _>>();
    demoted.entry("message").or_insert(Value::Null);
    if !demoted.contains_key("assistantMessageEvent") {
        demoted.insert("assistantMessageEvent".into(), demote_event(&Value::Null));
    }
    Value::Object(demoted)
}
#[derive(Default)]
pub struct SessionSnapshots {
    lifecycle: BTreeMap<String, String>,
    snapshots: BTreeMap<String, Vec<SnapshotRecord>>,
    questions: BTreeMap<String, serde_json::Map<String, Value>>,
}
impl SessionSnapshots {
    /// Whether [`Self::remember`] keeps `value` in the session's replay list (questions keep their
    /// own frame and `message_end` clears the list).
    fn retains(&self, session_id: &str, value: &Value) -> bool {
        match value["type"].as_str() {
            Some("message_start") => true,
            Some("message_update") if value["assistantMessageEvent"]["type"] == "text_start" => {
                true
            }
            Some("extension_ui_request")
                if value["method"] == "question" && value["id"].is_string() =>
            {
                false
            }
            Some("question_resolved" | "question_updated") if value["id"].is_string() => false,
            Some("message_end") => false,
            _ => self.snapshots.contains_key(session_id),
        }
    }
    pub fn remember(
        &mut self,
        session_id: &str,
        value: &Value,
        line: String,
        placeholder_line: Option<String>,
        source: Option<Value>,
    ) {
        let kind = value["type"].as_str();
        if kind == Some("session_lifecycle") {
            self.lifecycle.insert(session_id.into(), line);
            return;
        }
        let id = value["id"].as_str();
        if kind == Some("extension_ui_request")
            && value["method"] == "question"
            && let Some(id) = id
        {
            self.questions
                .entry(session_id.into())
                .or_default()
                .insert(id.into(), value.clone());
            return;
        }
        if kind == Some("question_resolved")
            && let Some(id) = id
        {
            if let Some(questions) = self.questions.get_mut(session_id) {
                questions.shift_remove(id);
            }
            return;
        }
        if kind == Some("question_updated")
            && let Some(id) = id
        {
            if let Some(frame) = self
                .questions
                .get_mut(session_id)
                .and_then(|questions| questions.get_mut(id))
            {
                frame["deadlineAtMs"] = value["deadlineAtMs"].clone();
                frame["remainingMs"] = value["remainingMs"].clone();
            }
            return;
        }
        let record = SnapshotRecord {
            line,
            placeholder_line,
            source,
            rendered: value[RENDERED_COMPONENT_RECORD] == true,
        };
        if kind == Some("message_start")
            || (kind == Some("message_update")
                && value["assistantMessageEvent"]["type"] == "text_start")
        {
            self.snapshots.insert(session_id.into(), vec![record]);
        } else if let Some(records) = self.snapshots.get_mut(session_id) {
            records.push(record);
        }
        if kind == Some("message_end") {
            self.snapshots.remove(session_id);
        }
    }
    pub fn replay(
        &mut self,
        session_id: &str,
        rendered: bool,
        placeholders: bool,
        now_ms: f64,
    ) -> Result<Vec<String>, serde_json::Error> {
        let mut lines = self
            .lifecycle
            .get(session_id)
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        for record in self.snapshots.get_mut(session_id).into_iter().flatten() {
            if record.rendered && !rendered {
                continue;
            }
            if placeholders && record.placeholder_line.is_none() {
                record.placeholder_line = Some(match &record.source {
                    Some(source) => crate::jsonl::serialize_json_line(
                        &crate::media_placeholders::omit_inline_media(source),
                    )?,
                    None => record.line.clone(),
                });
            }
            lines.push(if placeholders {
                record
                    .placeholder_line
                    .as_ref()
                    .unwrap_or(&record.line)
                    .clone()
            } else {
                record.line.clone()
            });
        }
        for frame in self
            .questions
            .get(session_id)
            .into_iter()
            .flat_map(serde_json::Map::values)
        {
            let mut frame = frame.clone();
            frame["remainingMs"] = Value::from(
                frame["deadlineAtMs"]
                    .as_f64()
                    .map_or(0., |deadline| (deadline - now_ms).max(0.)),
            );
            lines.push(crate::jsonl::serialize_json_line(&frame)?);
        }
        Ok(lines)
    }
    pub fn forget(&mut self, session_id: &str) {
        self.lifecycle.remove(session_id);
        self.snapshots.remove(session_id);
        self.questions.remove(session_id);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_broadcasts_to_observers_and_replays_latest_only() {
        let mut targets = ConnectionTargets::default();
        targets.register("observer");
        assert_eq!(
            targets.targets("s", None, false, false, Some("session_lifecycle")),
            vec![Some("observer".into())]
        );
        let mut snapshots = SessionSnapshots::default();
        for seq in [1, 2] {
            let record =
                json!({"type":"session_lifecycle","sessionId":"s","lifecycle":{"seq":seq}});
            snapshots.remember("s", &record, record.to_string(), None, None);
        }
        let replay = snapshots.replay("s", false, false, 0.).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&replay[0]).unwrap()["lifecycle"]["seq"],
            2
        );
    }
    #[test]
    fn source_is_kept_only_for_retained_media_records() {
        let mut fanout = SessionEventFanout::default();
        fanout
            .deliver("s", None, false, &json!({"type":"agent_end","messages":[]}))
            .unwrap();
        assert!(!fanout.snapshots.snapshots.contains_key("s"));
        fanout
            .deliver(
                "s",
                None,
                false,
                &json!({"type":"message_start","message":{}}),
            )
            .unwrap();
        fanout
            .deliver(
                "s",
                None,
                false,
                &json!({"type":"tool_execution_update","partialResult":{}}),
            )
            .unwrap();
        fanout
            .deliver("s", None, false, &json!({"type":"turn_end","message":{}}))
            .unwrap();
        let sources = fanout.snapshots.snapshots["s"]
            .iter()
            .map(|record| record.source.is_some())
            .collect::<Vec<_>>();
        assert_eq!(sources, vec![true, false, true]);
        let full = fanout.snapshots.replay("s", false, false, 0.).unwrap();
        assert_eq!(fanout.snapshots.replay("s", false, true, 0.).unwrap(), full);
    }
    #[test]
    fn demoted_snapshot_matches_clone_then_null_bytes() {
        let record = json!({"type":"message_update","message":{"content":"abc"},"assistantMessageEvent":{"type":"text_delta","partial":{"content":"abc"},"contentIndex":0,"delta":"c"},"sessionId":"s"});
        let mut expected = record.clone();
        expected["message"] = Value::Null;
        expected["assistantMessageEvent"]["partial"] = Value::Null;
        assert_eq!(
            serde_json::to_string(&demote_snapshot(&record)).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
        let mut without = record.clone();
        without["assistantMessageEvent"]
            .as_object_mut()
            .unwrap()
            .remove("partial");
        let mut expected = without.clone();
        expected["message"] = Value::Null;
        expected["assistantMessageEvent"]["partial"] = Value::Null;
        assert_eq!(
            serde_json::to_string(&demote_snapshot(&without)).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
    }
    #[test]
    fn pending_questions_replay_in_registration_order_after_update() {
        let mut snapshots = SessionSnapshots::default();
        for id in ["z", "a"] {
            snapshots.remember(
                "s",
                &json!({"type":"extension_ui_request","method":"question","id":id}),
                String::new(),
                None,
                None,
            );
        }
        snapshots.remember(
            "s",
            &json!({"type":"extension_ui_request","method":"question","id":"z","deadlineAtMs":100}),
            String::new(),
            None,
            None,
        );
        let frames = snapshots
            .replay("s", false, false, 0.)
            .unwrap()
            .into_iter()
            .map(|line| {
                serde_json::from_str::<Value>(&line).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(frames, vec!["z", "a"]);
    }
    #[test]
    fn targets_keep_connection_order_and_lifecycle_broadcast() {
        let mut targets = ConnectionTargets::default();
        assert_eq!(targets.targets("s", None, false, false, None), vec![None]);
        targets.register("z");
        targets.register("a");
        assert!(targets.attach("z", "s"));
        assert!(!targets.attach("z", "s"));
        assert_eq!(
            targets.targets("s", None, false, false, None),
            vec![Some("z".into())]
        );
        assert!(targets.targets("s", None, false, true, None).is_empty());
        assert!(targets.set_capabilities("z", &["rendered_components".into()]));
        assert_eq!(
            targets.targets("s", None, false, true, None),
            vec![Some("z".into())]
        );
        assert_eq!(
            targets.targets("s", None, false, false, Some("agent_idle")),
            vec![Some("z".into()), Some("a".into())]
        );
        assert_eq!(
            targets.targets("s", Some("unknown"), true, false, None),
            vec![Some("unknown".into())]
        );
    }
    #[test]
    fn message_end_discards_replay_and_rendered_requires_capability() {
        let mut snapshots = SessionSnapshots::default();
        let value = json!({"type":"message_start","__senpiRenderedComponent":true});
        snapshots.remember("s", &value, "start\n".into(), None, None);
        assert!(snapshots.replay("s", false, false, 0.).unwrap().is_empty());
        assert_eq!(
            snapshots.replay("s", true, false, 0.).unwrap(),
            vec!["start\n"]
        );
        snapshots.remember(
            "s",
            &json!({"type":"message_end"}),
            "end\n".into(),
            None,
            None,
        );
        assert!(snapshots.replay("s", true, false, 0.).unwrap().is_empty());
    }
    #[test]
    fn question_replay_uses_updated_deadline_and_resolution_removes_it() {
        let mut snapshots = SessionSnapshots::default();
        snapshots.remember(
            "s",
            &json!({"type":"extension_ui_request","method":"question","id":"q","deadlineAtMs":100}),
            String::new(),
            None,
            None,
        );
        snapshots.remember(
            "s",
            &json!({"type":"question_updated","id":"q","deadlineAtMs":200,"remainingMs":150}),
            String::new(),
            None,
            None,
        );
        let line = snapshots.replay("s", false, false, 150.).unwrap().remove(0);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["remainingMs"], 50.);
        snapshots.remember(
            "s",
            &json!({"type":"question_resolved","id":"q"}),
            String::new(),
            None,
            None,
        );
        assert!(
            snapshots
                .replay("s", false, false, 300.)
                .unwrap()
                .is_empty()
        );
    }

    fn test_actor(
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

    #[tokio::test]
    async fn grant_select_routes_to_capable_attached_connection_only() {
        let mut fanout = SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);
        let (w2, _r2) = tokio::io::duplex(1024);

        fanout.register("question-client", test_actor(w1));
        fanout
            .set_capabilities("question-client", &["question".into()])
            .unwrap();
        fanout.attach("question-client", "s1", 0.0).unwrap();

        fanout.register("grant-client", test_actor(w2));
        fanout
            .set_capabilities("grant-client", &["grant_select".into()])
            .unwrap();
        fanout.attach("grant-client", "s1", 0.0).unwrap();

        let grant_dialog = json!({
            "type": "extension_ui_request",
            "id": "grant-req-1",
            "method": "select",
            "grantSelect": true,
            "title": "Select grant"
        });

        let stdio = fanout.deliver("s1", None, false, &grant_dialog).unwrap();
        assert!(stdio.is_empty());

        // Grant is assigned to grant-client
        assert_eq!(
            fanout.grants_for_connection("grant-client"),
            vec![("s1".into(), "grant-req-1".into())]
        );
        assert!(fanout.grants_for_connection("question-client").is_empty());

        // Answering from question-client is rejected
        assert!(!fanout.accept_grant_response("s1", "grant-req-1", Some("question-client")));
        // Answering from grant-client is accepted
        assert!(fanout.accept_grant_response("s1", "grant-req-1", Some("grant-client")));
        // After acceptance, the grant is cleared
        assert!(fanout.grants_for_connection("grant-client").is_empty());
    }

    #[tokio::test]
    async fn grant_select_dropped_when_no_attached_connection_has_capability() {
        let mut fanout = SessionEventFanout::default();
        let (w, _r) = tokio::io::duplex(1024);

        fanout.register("headless-client", test_actor(w));
        fanout
            .set_capabilities("headless-client", &["question".into()])
            .unwrap();
        fanout.attach("headless-client", "s1", 0.0).unwrap();

        let grant_dialog = json!({
            "type": "extension_ui_request",
            "id": "grant-req-2",
            "method": "select",
            "grantSelect": true,
            "title": "Select grant"
        });

        // Dropped because non-empty fanout has no capable attached connections
        let stdio = fanout.deliver("s1", None, false, &grant_dialog).unwrap();
        assert!(stdio.is_empty());
        assert!(fanout.grants_for_connection("headless-client").is_empty());
    }

    #[tokio::test]
    async fn grant_select_falls_back_to_stdio_when_fanout_is_empty() {
        let mut fanout = SessionEventFanout::default();
        let grant_dialog = json!({
            "type": "extension_ui_request",
            "id": "grant-req-3",
            "method": "select",
            "grantSelect": true,
            "title": "Select grant"
        });

        // When fanout is empty, deliver returns the record for stdio
        let stdio = fanout.deliver("s1", None, false, &grant_dialog).unwrap();
        assert_eq!(stdio.len(), 1);
        assert_eq!(stdio[0]["id"], "grant-req-3");
    }

    #[tokio::test]
    async fn security_notices_replay_and_cleanup() {
        let mut fanout = SessionEventFanout::default();
        let (w1, _r1) = tokio::io::duplex(1024);
        fanout.register("c1", test_actor(w1));
        fanout.attach("c1", "s1", 0.0).unwrap();

        let snapshot_notice = json!({
            "type": "security_notices",
            "snapshot": {
                "generation": 1,
                "notices": [
                    {"category": "skill-link-blocked", "message": "Notice Alpha"}
                ]
            }
        });
        let res = fanout.deliver("s1", None, false, &snapshot_notice).unwrap();
        assert!(res.is_empty());
        assert!(fanout.security_snapshot("s1").is_some());

        // Late attacher gets replay
        let (w2, _r2) = tokio::io::duplex(1024);
        fanout.register("c2", test_actor(w2));
        fanout.attach("c2", "s1", 0.0).unwrap();

        // Detach and forget cleans state
        fanout.detach("c1", "s1");
        fanout.unregister("c2");
        fanout.forget_session("s1");
        assert!(fanout.security_snapshot("s1").is_none());
        assert!(fanout.attached_connections("s1").is_empty());
    }
}
