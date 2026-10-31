//! The separate authenticated presenter control endpoint (senpi `mcp-presenter-control.ts`). Ordinary RPC
//! credentials cannot enroll or acquire: every command is a signed challenge/response bound to the host
//! incarnation, the principal's backend generation and a fresh nonce.
use crate::mcp_form_dialog::lock;
use crate::mcp_form_types::{McpFormBinding, McpPresenterKind};
use crate::mcp_presenter_authority::{
    McpPresenterAuthority, McpPresenterConnection, McpPresenterOwner,
};
use crate::mcp_presenter_enrollment::{McpPresenterEnrollment, PresenterPrincipal, random_hex32};
use crate::mcp_presenter_successor::{LaunchOwner, McpPresenterSuccessors};
use crate::mcp_presenter_wire::{BoxedPrivateStream, PrivateChannel};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

const MAX_CONTROLS: usize = 64;
const CHALLENGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// What the control endpoint reads from the host's event writer (senpi `SessionEventWriter` presenter methods).
pub trait McpPresenterWriter: Send + Sync {
    fn mcp_form_binding(&self, session_id: &str) -> Option<Arc<McpFormBinding>>;
    fn find_mcp_presenter_connection(&self, nonce: &str) -> Option<McpPresenterConnection>;
    fn connection_capabilities(&self, connection_id: &str) -> Option<Vec<String>>;
}

struct Principal {
    principal: PresenterPrincipal,
    /// Bindings a signed native-supervisor decision assigned (senpi's `WeakSet<McpFormBinding>`).
    sessions: Mutex<BTreeSet<u64>>,
}

#[derive(Default)]
struct ControlState {
    principals: BTreeMap<String, Arc<Principal>>,
    controls: Vec<PrivateChannel>,
    revokes: BTreeMap<u64, (PrivateChannel, Arc<dyn Fn() + Send + Sync>)>,
    launch_owner: Option<(PrivateChannel, Arc<Principal>, String)>,
    backend_generations: Vec<(PrivateChannel, Arc<Principal>, String)>,
    next: u64,
}

struct Shared {
    writer: Arc<dyn McpPresenterWriter>,
    authority: McpPresenterAuthority,
    state: Mutex<ControlState>,
    enrollment: McpPresenterEnrollment,
    successors: McpPresenterSuccessors,
}

/// senpi `McpPresenterControl`.
pub struct McpPresenterControl {
    shared: Arc<Shared>,
    pub path: String,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Default)]
struct Session {
    principal: Option<Arc<Principal>>,
    owner: Option<Arc<McpPresenterOwner>>,
    transcript: Option<String>,
    pending: Option<Map<String, Value>>,
    backend_generation: Option<String>,
    deadline: Option<tokio::time::Instant>,
    timer: Option<tokio::task::JoinHandle<()>>,
    registrations: BTreeMap<String, ()>,
}

impl McpPresenterControl {
    pub fn new(
        writer: Arc<dyn McpPresenterWriter>,
        authority: McpPresenterAuthority,
        path: &str,
    ) -> Self {
        let enrollment = McpPresenterEnrollment::new(authority.instance(), path);
        let successors =
            McpPresenterSuccessors::new(authority.instance(), Arc::clone(&enrollment.nonces));
        Self {
            shared: Arc::new(Shared {
                writer,
                authority,
                state: Mutex::default(),
                enrollment,
                successors,
            }),
            path: path.to_owned(),
            listener: Mutex::new(None),
        }
    }

    /// Bind and serve the private control socket (owner-only; the directory is private).
    #[cfg(unix)]
    pub async fn listen(&self) -> std::io::Result<()> {
        let listener = tokio::net::UnixListener::bind(&self.path)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        let shared = Arc::clone(&self.shared);
        *lock(&self.listener) = Some(tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                Shared::manage(&shared, Box::new(stream));
            }
        }));
        Ok(())
    }
    #[cfg(not(unix))]
    pub async fn listen(&self) -> std::io::Result<()> {
        Err(std::io::Error::other(
            "presenter control endpoint is unix-only",
        ))
    }

    /// Serve one already-connected control stream (the accept loop's body; also the test seam).
    pub fn manage(&self, stream: BoxedPrivateStream) {
        Shared::manage(&self.shared, stream);
    }

    /// A deferred handover is spawned by the old host, not the CLI that already returned.
    pub async fn successor_bootstrap(&self) -> Option<BoxedPrivateStream> {
        let owner = lock(&self.shared.state).launch_owner.clone();
        let (stream, principal, backend_generation) = owner?;
        self.shared
            .successors
            .prepare(LaunchOwner {
                stream,
                principal: principal.principal.clone(),
                backend_generation,
            })
            .await
    }

    /// Only the inherited launch channel may introduce the installation public key.
    pub fn bootstrap(&self, stream: BoxedPrivateStream) {
        let shared = Arc::downgrade(&self.shared);
        self.shared.enrollment.bootstrap(stream, move |principal| {
            if let Some(shared) = shared.upgrade() {
                lock(&shared.state).principals.insert(
                    principal.id.clone(),
                    Arc::new(Principal {
                        principal,
                        sessions: Mutex::default(),
                    }),
                );
            }
        });
    }

    pub fn dispose(&self) {
        self.shared.authority.dispose();
        let controls = std::mem::take(&mut lock(&self.shared.state).controls);
        for stream in controls {
            stream.destroy();
        }
        self.shared.successors.dispose();
        self.shared.enrollment.dispose();
        lock(&self.shared.state).principals.clear();
        if let Some(listener) = lock(&self.listener).take() {
            listener.abort();
        }
    }
}

/// senpi `send(stream, { type, id, ...reply })` through `JSON.stringify`: `id` follows `type`, and a command
/// without a string id gets no `id` key at all.
fn with_reply_id(answer: Value, id: Option<&str>) -> Value {
    let Value::Object(fields) = answer else {
        return answer;
    };
    let mut reply = Map::new();
    let mut fields = fields.into_iter();
    if let Some((key, value)) = fields.next() {
        reply.insert(key, value);
    }
    if let Some(id) = id {
        reply.insert("id".to_owned(), id.into());
    }
    reply.extend(fields);
    Value::Object(reply)
}
fn not_authorized() -> &'static str {
    crate::mcp_presenter_authority::PRESENTER_NOT_AUTHORIZED
}

impl Shared {
    fn manage(shared: &Arc<Self>, stream: BoxedPrivateStream) {
        if lock(&shared.state).controls.len() >= MAX_CONTROLS {
            drop(stream);
            return;
        }
        let session = Arc::new(Mutex::new(Session::default()));
        let weak = Arc::downgrade(shared);
        let receiver = Arc::clone(&session);
        let channel = PrivateChannel::open(
            stream,
            move |channel, value| {
                if let Some(shared) = weak.upgrade() {
                    shared.receive(channel, &receiver, value);
                }
            },
            false,
        );
        let key = {
            let mut state = lock(&shared.state);
            state.next += 1;
            state.controls.push(channel.clone());
            state.next
        };
        let revoke: Arc<dyn Fn() + Send + Sync> = {
            let (weak, session, channel) = (
                Arc::downgrade(shared),
                Arc::clone(&session),
                channel.clone(),
            );
            Arc::new(move || {
                let Some(shared) = weak.upgrade() else { return };
                let (owner, registrations, timer) = {
                    let mut session = lock(&session);
                    (
                        session.owner.clone(),
                        std::mem::take(&mut session.registrations),
                        session.timer.take(),
                    )
                };
                if let Some(timer) = timer {
                    timer.abort();
                }
                if let Some(owner) = owner {
                    for registration in registrations.keys() {
                        owner.revoke(registration);
                    }
                }
                {
                    let mut state = lock(&shared.state);
                    state.controls.retain(|control| *control != channel);
                    state.revokes.remove(&key);
                    state
                        .backend_generations
                        .retain(|(stream, _, _)| *stream != channel);
                    if state
                        .launch_owner
                        .as_ref()
                        .is_some_and(|(stream, _, _)| *stream == channel)
                    {
                        state.launch_owner = None;
                    }
                }
                shared.successors.revoke(&channel);
            })
        };
        lock(&shared.state)
            .revokes
            .insert(key, (channel.clone(), Arc::clone(&revoke)));
        let expired = channel.clone();
        lock(&session).timer = Some(tokio::spawn(async move {
            tokio::time::sleep(CHALLENGE_DEADLINE).await;
            expired.destroy();
        }));
        channel.on_close(move || revoke());
    }

    fn arm(session: &mut Session, channel: &PrivateChannel) {
        if let Some(timer) = session.timer.take() {
            timer.abort();
        }
        let expired = channel.clone();
        session.timer = Some(tokio::spawn(async move {
            tokio::time::sleep(CHALLENGE_DEADLINE).await;
            expired.destroy();
        }));
    }

    fn receive(&self, channel: &PrivateChannel, session: &Arc<Mutex<Session>>, value: Value) {
        let Some(record) = value.as_object() else {
            channel.destroy();
            return;
        };
        if self.successors.receive(channel, record) {
            return;
        }
        if record.get("type").and_then(Value::as_str) == Some("challenge") {
            let mut state = lock(session);
            let (Some(principal_id), Some(backend), Some(command)) = (
                record.get("principalId").and_then(Value::as_str),
                record.get("backendGeneration").and_then(Value::as_str),
                record.get("command").and_then(Value::as_object),
            ) else {
                drop(state);
                channel.destroy();
                return;
            };
            if state.pending.is_some() {
                drop(state);
                channel.destroy();
                return;
            }
            let requested = lock(&self.state).principals.get(principal_id).cloned();
            let Some(requested) = requested.filter(|requested| {
                state
                    .principal
                    .as_ref()
                    .is_none_or(|current| Arc::ptr_eq(current, requested))
            }) else {
                drop(state);
                channel.destroy();
                return;
            };
            state.principal = Some(requested);
            state.backend_generation = Some(backend.to_owned());
            state.pending = Some(command.clone());
            state.deadline = Some(tokio::time::Instant::now() + CHALLENGE_DEADLINE);
            Self::arm(&mut state, channel);
            let transcript = json!([
                "mcp-presenter-control-v1",
                self.authority.instance(),
                backend,
                uuid::Uuid::new_v4().to_string(),
                random_hex32(),
                command,
            ])
            .to_string();
            state.transcript = Some(transcript.clone());
            drop(state);
            channel.send(&json!({"type":"challenge","transcript":transcript,"hostInstance":self.authority.instance()}));
            return;
        }
        let (principal, command, backend, owner) = {
            let mut state = lock(session);
            let proven = match (
                &state.principal,
                &state.pending,
                &state.transcript,
                state.deadline,
            ) {
                (Some(principal), Some(_), Some(transcript), Some(deadline)) => {
                    tokio::time::Instant::now() < deadline
                        && crate::mcp_presenter_wire::signed(
                            &value,
                            transcript,
                            &principal.principal.key,
                        )
                }
                _ => false,
            };
            if !proven {
                drop(state);
                channel.destroy();
                return;
            }
            if let Some(timer) = state.timer.take() {
                timer.abort();
            }
            let command = state.pending.take().unwrap_or_default();
            state.transcript = None;
            let principal = state.principal.clone();
            let Some(principal) = principal else { return };
            let owner = state
                .owner
                .get_or_insert_with(|| {
                    Arc::new(self.authority.enroll(Some(principal.principal.id.clone())))
                })
                .clone();
            let Some(backend) = state.backend_generation.clone() else {
                drop(state);
                channel.destroy();
                return;
            };
            (principal, command, backend, owner)
        };
        let superseded: Vec<_> = {
            let state = lock(&self.state);
            state
                .backend_generations
                .iter()
                .filter(|(_, old, generation)| {
                    Arc::ptr_eq(old, &principal) && *generation != backend
                })
                .filter_map(|(stream, _, _)| {
                    state
                        .revokes
                        .values()
                        .find(|(control, _)| control == stream)
                        .map(|(control, revoke)| (control.clone(), Arc::clone(revoke)))
                })
                .collect()
        };
        for (stream, revoke) in superseded {
            revoke();
            stream.destroy();
        }
        {
            let mut state = lock(&self.state);
            state
                .backend_generations
                .retain(|(stream, _, _)| stream != channel);
            state.backend_generations.push((
                channel.clone(),
                Arc::clone(&principal),
                backend.clone(),
            ));
            state.launch_owner = Some((channel.clone(), Arc::clone(&principal), backend));
        }
        let id = command.get("id").and_then(Value::as_str);
        let answer = match self.dispatch(&command, &principal, &owner, session) {
            Ok(answer) => answer,
            Err(_) => json!({"type":"mcp_presenter_error","error":not_authorized()}),
        };
        channel.send(&with_reply_id(answer, id));
    }

    fn dispatch(
        &self,
        command: &Map<String, Value>,
        principal: &Arc<Principal>,
        owner: &Arc<McpPresenterOwner>,
        session: &Arc<Mutex<Session>>,
    ) -> Result<Value, &'static str> {
        let text = |key: &str| command.get(key).and_then(Value::as_str);
        match text("type") {
            Some("mcp_presenter_assign") => {
                let session_id = text("sessionId").ok_or(not_authorized())?;
                let binding = self
                    .writer
                    .mcp_form_binding(session_id)
                    .ok_or(not_authorized())?;
                if text("bindingEpoch") != Some(binding.epoch.as_str())
                    || text("purpose") != Some("mcp_form")
                {
                    return Err(not_authorized());
                }
                // Only a signed native-supervisor decision assigns an opened/adopted session. Enrollment and
                // ordinary RPC attachment do not imply assignment.
                lock(&principal.sessions).insert(binding.id);
                Ok(json!({"type":"mcp_presenter_assigned","sessionId":session_id}))
            }
            Some("mcp_presenter_acquire") => {
                let kind = match text("kind") {
                    Some("desktop") => McpPresenterKind::Desktop,
                    Some("gateway") => McpPresenterKind::Gateway,
                    _ => return Err(not_authorized()),
                };
                let (Some(session_id), Some(epoch), Some(nonce)) = (
                    text("sessionId"),
                    text("bindingEpoch"),
                    text("connectionNonce"),
                ) else {
                    return Err(not_authorized());
                };
                if text("hostInstance") != Some(self.authority.instance()) {
                    return Err(not_authorized());
                }
                let binding = self
                    .writer
                    .mcp_form_binding(session_id)
                    .ok_or(not_authorized())?;
                let connection = self
                    .writer
                    .find_mcp_presenter_connection(nonce)
                    .ok_or(not_authorized())?;
                if !lock(&principal.sessions).contains(&binding.id) || binding.epoch != epoch {
                    return Err(not_authorized());
                }
                let capabilities = self
                    .writer
                    .connection_capabilities(&connection.id)
                    .unwrap_or_default();
                if !capabilities.iter().any(|value| value == "interactive_user")
                    || !capabilities
                        .iter()
                        .any(|value| value == "mcp_form_presenter_v1")
                {
                    return Err(not_authorized());
                }
                let context = self
                    .authority
                    .connection_context(&connection, session_id)
                    .ok_or(not_authorized())?;
                if text("purpose") != Some("mcp_form")
                    || text("attachmentId") != Some(context.attachment_id.as_str())
                    || text("attachmentEpoch") != Some(context.attachment_epoch.as_str())
                {
                    return Err(not_authorized());
                }
                let registration = owner.register(&binding, &connection, kind)?;
                let grant = owner.acquire(&registration)?;
                lock(session).registrations.insert(registration.clone(), ());
                Ok(json!({
                    "type": "mcp_presenter_acquired",
                    "registrationId": registration,
                    "grantId": grant.grant_id,
                    "scope": grant.scope,
                }))
            }
            Some("mcp_presenter_revoke") => {
                let registration = text("registrationId")
                    .filter(|id| lock(session).registrations.contains_key(*id))
                    .ok_or(not_authorized())?
                    .to_owned();
                owner.revoke(&registration);
                lock(session).registrations.remove(&registration);
                Ok(json!({"type":"mcp_presenter_revoked"}))
            }
            _ => Err(not_authorized()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_presenter_enrollment::tests::{Peer, enrollment_proof, enrollment_request};

    struct Writer {
        authority: McpPresenterAuthority,
        binding: Arc<McpFormBinding>,
        connection: McpPresenterConnection,
        capabilities: Vec<String>,
    }
    impl McpPresenterWriter for Writer {
        fn mcp_form_binding(&self, session_id: &str) -> Option<Arc<McpFormBinding>> {
            (session_id == "session").then(|| Arc::clone(&self.binding))
        }
        fn find_mcp_presenter_connection(&self, nonce: &str) -> Option<McpPresenterConnection> {
            (nonce == self.connection.nonce).then(|| self.connection.clone())
        }
        fn connection_capabilities(&self, _: &str) -> Option<Vec<String>> {
            let _ = &self.authority;
            Some(self.capabilities.clone())
        }
    }

    async fn signed_command(peer: &mut Peer, principal: &str, command: Value) -> Value {
        peer.send(&json!({"type":"challenge","principalId":principal,"backendGeneration":"backend-4254","command":command}))
            .await;
        let challenge = peer.next().await.unwrap();
        peer.send(&enrollment_proof(challenge["transcript"].as_str().unwrap()))
            .await;
        peer.next().await.unwrap()
    }

    #[tokio::test]
    async fn only_protected_enrollment_and_a_scope_bound_signature_can_acquire() {
        let authority = McpPresenterAuthority::new(None);
        let binding = authority.bind("session", "durable", Arc::new(|| {}));
        let connection = authority.connect("rpc-1", Arc::new(|_| true));
        authority.attach(&connection, &binding);
        let writer = Arc::new(Writer {
            authority: authority.clone(),
            binding: Arc::clone(&binding),
            connection: connection.clone(),
            capabilities: vec!["interactive_user".into(), "mcp_form_presenter_v1".into()],
        });
        let control = McpPresenterControl::new(writer, authority.clone(), "unused");
        // Enrollment over the launch channel.
        let (launch, host) = tokio::io::duplex(1 << 16);
        control.bootstrap(Box::new(host));
        let mut launch = Peer::new(launch);
        launch.send(&enrollment_request(json!({}))).await;
        let challenge = launch.next().await.unwrap();
        launch
            .send(&enrollment_proof(challenge["transcript"].as_str().unwrap()))
            .await;
        let enrolled = launch.next().await.unwrap();
        let principal = enrolled["principalId"].as_str().unwrap().to_owned();
        // An unknown principal is refused before any challenge.
        let (peer, host) = tokio::io::duplex(1 << 16);
        control.manage(Box::new(host));
        let mut stranger = Peer::new(peer);
        stranger
            .send(&json!({"type":"challenge","principalId":"other","backendGeneration":"b","command":{}}))
            .await;
        stranger.eof("unknown principal").await;
        let (peer, host) = tokio::io::duplex(1 << 16);
        control.manage(Box::new(host));
        let mut peer = Peer::new(peer);
        let context = authority
            .connection_context(&connection, "session")
            .unwrap();
        let acquire = json!({
            "type":"mcp_presenter_acquire","id":"1","hostInstance":authority.instance(),"sessionId":"session",
            "bindingEpoch":binding.epoch,"connectionNonce":context.connection_nonce,"kind":"desktop",
            "purpose":"mcp_form","attachmentId":context.attachment_id,"attachmentEpoch":context.attachment_epoch,
        });
        // Unassigned: refused.
        let refused = signed_command(&mut peer, &principal, acquire.clone()).await;
        assert_eq!(
            refused.to_string(),
            r#"{"type":"mcp_presenter_error","id":"1","error":"presenter_not_authorized"}"#
        );
        // A command without an id is answered without an `id` key (senpi `JSON.stringify` drops `undefined`).
        let mut anonymous = acquire.clone();
        anonymous.as_object_mut().unwrap().remove("id");
        let refused = signed_command(&mut peer, &principal, anonymous).await;
        assert_eq!(
            refused.to_string(),
            r#"{"type":"mcp_presenter_error","error":"presenter_not_authorized"}"#
        );
        let assigned = signed_command(
            &mut peer,
            &principal,
            json!({"type":"mcp_presenter_assign","id":"2","sessionId":"session","bindingEpoch":binding.epoch,"purpose":"mcp_form"}),
        )
        .await;
        assert_eq!(assigned["type"], "mcp_presenter_assigned");
        let acquired = signed_command(&mut peer, &principal, acquire.clone()).await;
        assert_eq!(acquired["type"], "mcp_presenter_acquired");
        assert_eq!(acquired["scope"]["presenterKind"], "desktop");
        assert!(binding.capture().is_some());
        // A replayed proof over a fresh challenge transcript fails.
        peer.send(&json!({"type":"challenge","principalId":principal,"backendGeneration":"backend-4254","command":acquire}))
            .await;
        peer.next().await.unwrap();
        peer.send(&enrollment_proof("stale transcript")).await;
        // Closing the control stream revokes what it acquired: `destroy` runs its close callbacks before the
        // aborted tasks drop the stream, so the revocation precedes the EOF observed here.
        peer.eof("stale proof").await;
        assert!(binding.capture().is_none());
        control.dispose();
    }
}
