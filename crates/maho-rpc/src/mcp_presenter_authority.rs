//! Host-issued MCP presenter authority (senpi `mcp-presenter-authority.ts`): epoch-bound bindings, signed
//! principal registrations, one grant per binding, and the cancellation/readiness fences every private frame
//! is checked against.
//!
//! Host-owned only. No public session command can call enrollment or install a registration.
use crate::mcp_form_dialog::{
    McpFormClock, McpFormDialog, McpFormDialogContext, McpFormEnvelope, McpPresenterReply,
    Termination, lock,
};
use crate::mcp_form_types::{
    Listener, McpFormBinding, McpFormInput, McpFormOutcome, McpFormPresentation, McpFormScope,
    McpPresenterKind,
};
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

pub const PRESENTER_NOT_AUTHORIZED: &str = "presenter_not_authorized";

pub type Sender = Arc<dyn Fn(&Value) -> bool + Send + Sync>;
type Admit = Arc<dyn Fn() + Send + Sync>;

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// senpi `McpPresenterConnection`: what the writer and the control endpoint may hold. `key` is its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPresenterConnection {
    pub key: u64,
    pub id: String,
    pub epoch: String,
    pub nonce: String,
}
struct Attachment {
    id: String,
    epoch: String,
}
struct Connection {
    info: McpPresenterConnection,
    send: Sender,
    attachments: BTreeMap<u64, Attachment>,
}
struct Binding {
    session_id: String,
    durable_id: String,
    epoch: String,
    lifetime: AbortController,
    admit: Admit,
    listeners: BTreeMap<u64, Listener>,
    grant: Option<Arc<Grant>>,
}
struct Registration {
    principal: String,
    generation: String,
    binding: u64,
    connection: u64,
    kind: McpPresenterKind,
}
struct Grant {
    id: String,
    registration: String,
    binding: u64,
    connection: u64,
    scope: McpFormScope,
    lifetime: AbortController,
    dialogs: Mutex<Vec<(String, Arc<McpFormDialog>)>>,
    /// senpi `queuedForm(grant, ...)`: one presented form at a time per grant, FIFO.
    queue: tokio::sync::Mutex<()>,
}

/// The `connectionContext` an attached presenter reads before it asks for a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPresenterContext {
    pub host_instance: String,
    pub connection_nonce: String,
    pub binding_epoch: String,
    pub attachment_id: String,
    pub attachment_epoch: String,
}
impl McpPresenterContext {
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "hostInstance": self.host_instance,
            "connectionNonce": self.connection_nonce,
            "bindingEpoch": self.binding_epoch,
            "attachmentId": self.attachment_id,
            "attachmentEpoch": self.attachment_epoch,
        })
    }
}

#[derive(Default)]
struct Inner {
    connections: BTreeMap<u64, Connection>,
    bindings: BTreeMap<u64, Binding>,
    sessions: BTreeMap<String, u64>,
    registrations: BTreeMap<String, Registration>,
    next: u64,
}
impl Inner {
    fn key(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn binding_for(&self, session_id: &str) -> Option<(u64, &Binding)> {
        let key = *self.sessions.get(session_id)?;
        self.bindings.get(&key).map(|binding| (key, binding))
    }
    fn current(&self, grant: &Arc<Grant>) -> bool {
        let Some(binding) = self.bindings.get(&grant.binding) else {
            return false;
        };
        self.registrations.contains_key(&grant.registration)
            && !binding.lifetime.signal().aborted()
            && !grant.lifetime.signal().aborted()
            && self.sessions.get(&binding.session_id) == Some(&grant.binding)
            && binding
                .grant
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, grant))
            && self
                .connections
                .get(&grant.connection)
                .and_then(|connection| connection.attachments.get(&grant.binding))
                .is_some_and(|attachment| attachment.epoch == grant.scope.attachment_epoch)
    }
}

/// Work a revocation leaves for after the lock: listeners to notify, entries to terminate, a lifetime to end.
#[derive(Default)]
struct Revoked {
    /// Binding lifetimes end first, as senpi's `binding.lifetime.abort()` precedes the revoke.
    bindings: Vec<AbortController>,
    listeners: Vec<Listener>,
    terminations: Vec<(Arc<McpFormDialog>, Termination)>,
    lifetimes: Vec<AbortController>,
}
impl Revoked {
    fn run(self) {
        for lifetime in self.bindings {
            lifetime.abort(None);
        }
        for listener in self.listeners {
            listener();
        }
        // Retain entry references after authority is fenced. Cleanup never validates the revoked sender.
        for (dialog, cause) in self.terminations {
            dialog.terminate(cause);
        }
        for lifetime in self.lifetimes {
            lifetime.abort(None);
        }
    }
}

struct Shared {
    instance: String,
    clock: Arc<dyn McpFormClock>,
    inner: Mutex<Inner>,
}
impl Shared {
    fn revoke(inner: &mut Inner, id: &str, operation: bool, revoked: &mut Revoked) {
        let Some(registration) = inner.registrations.remove(id) else {
            return;
        };
        let Some(binding) = inner.bindings.get_mut(&registration.binding) else {
            return;
        };
        let Some(grant) = binding.grant.clone() else {
            return;
        };
        if grant.registration != id {
            return;
        }
        binding.grant = None;
        revoked
            .listeners
            .extend(binding.listeners.values().cloned());
        let cause = if operation {
            Termination::OperationAborted
        } else {
            Termination::PresentationLost
        };
        revoked.terminations.extend(
            lock(&grant.dialogs)
                .iter()
                .map(|(_, dialog)| (Arc::clone(dialog), cause)),
        );
        revoked.lifetimes.push(grant.lifetime.clone());
    }
}

/// The handle `enroll` returns: the only path that registers, acquires or revokes a presenter.
pub struct McpPresenterOwner {
    shared: Weak<Shared>,
    principal: String,
    generation: String,
}
/// What `acquire` hands the authenticated owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPresenterGrant {
    pub grant_id: String,
    pub scope: McpFormScope,
}

impl McpPresenterOwner {
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn register(
        &self,
        binding: &McpFormBinding,
        sender: &McpPresenterConnection,
        kind: McpPresenterKind,
    ) -> Result<String, &'static str> {
        let shared = self.shared.upgrade().ok_or(PRESENTER_NOT_AUTHORIZED)?;
        let mut inner = lock(&shared.inner);
        let attached = inner
            .bindings
            .get(&binding.id)
            .is_some_and(|entry| entry.grant.is_none())
            && inner
                .connections
                .get(&sender.key)
                .is_some_and(|connection| connection.attachments.contains_key(&binding.id));
        if !attached {
            return Err(PRESENTER_NOT_AUTHORIZED);
        }
        let id = uuid();
        inner.registrations.insert(
            id.clone(),
            Registration {
                principal: self.principal.clone(),
                generation: self.generation.clone(),
                binding: binding.id,
                connection: sender.key,
                kind,
            },
        );
        Ok(id)
    }
    pub fn acquire(&self, registration: &str) -> Result<McpPresenterGrant, &'static str> {
        let shared = self.shared.upgrade().ok_or(PRESENTER_NOT_AUTHORIZED)?;
        let listeners = {
            let mut inner = lock(&shared.inner);
            let Some(entry) = inner.registrations.get(registration) else {
                return Err(PRESENTER_NOT_AUTHORIZED);
            };
            if entry.principal != self.principal {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let (binding_key, connection_key, kind, generation) = (
                entry.binding,
                entry.connection,
                entry.kind,
                entry.generation.clone(),
            );
            let Some(connection) = inner.connections.get(&connection_key) else {
                return Err(PRESENTER_NOT_AUTHORIZED);
            };
            let Some(attachment) = connection.attachments.get(&binding_key) else {
                return Err(PRESENTER_NOT_AUTHORIZED);
            };
            let Some(binding) = inner.bindings.get(&binding_key) else {
                return Err(PRESENTER_NOT_AUTHORIZED);
            };
            if binding.lifetime.signal().aborted() || binding.grant.is_some() {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let scope = McpFormScope {
                host_instance: shared.instance.clone(),
                durable_session_id: binding.durable_id.clone(),
                binding_epoch: binding.epoch.clone(),
                attachment_id: attachment.id.clone(),
                attachment_epoch: attachment.epoch.clone(),
                connection_id: connection.info.id.clone(),
                connection_epoch: connection.info.epoch.clone(),
                presenter_principal_id: self.principal.clone(),
                presenter_generation: generation,
                presentation_binding_id: registration.to_owned(),
                presenter_kind: kind,
                purpose: "mcp_form".into(),
            };
            let grant = Arc::new(Grant {
                id: uuid(),
                registration: registration.to_owned(),
                binding: binding_key,
                connection: connection_key,
                scope: scope.clone(),
                lifetime: AbortController::new(),
                dialogs: Mutex::default(),
                queue: tokio::sync::Mutex::new(()),
            });
            let grant_id = grant.id.clone();
            let binding = inner
                .bindings
                .get_mut(&binding_key)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            binding.grant = Some(grant);
            (
                binding.listeners.values().cloned().collect::<Vec<_>>(),
                McpPresenterGrant { grant_id, scope },
            )
        };
        for listener in listeners.0 {
            listener();
        }
        Ok(listeners.1)
    }
    pub fn revoke(&self, registration: &str) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut revoked = Revoked::default();
        {
            let mut inner = lock(&shared.inner);
            if inner
                .registrations
                .get(registration)
                .is_some_and(|entry| entry.principal == self.principal)
            {
                Shared::revoke(&mut inner, registration, false, &mut revoked);
            }
        }
        revoked.run();
    }
}

/// senpi `McpPresenterAuthority`.
#[derive(Clone)]
pub struct McpPresenterAuthority {
    shared: Arc<Shared>,
}
impl Default for McpPresenterAuthority {
    fn default() -> Self {
        Self::new(None)
    }
}

impl McpPresenterAuthority {
    pub fn new(clock: Option<Arc<dyn McpFormClock>>) -> Self {
        Self {
            shared: Arc::new(Shared {
                // The host process this authority belongs to: its protocol `instanceId`, so a launcher can name the
                // host it replaces.
                instance: crate::protocol_identity::host_instance_id().to_owned(),
                clock: clock.unwrap_or_else(crate::mcp_form_dialog::mcp_form_clock),
                inner: Mutex::default(),
            }),
        }
    }
    pub fn instance(&self) -> &str {
        &self.shared.instance
    }

    pub fn dispose(&self) {
        let bindings: Vec<u64> = lock(&self.shared.inner).bindings.keys().copied().collect();
        for key in bindings {
            self.dispose_binding(key);
        }
        let mut inner = lock(&self.shared.inner);
        inner.connections.clear();
        inner.registrations.clear();
    }

    pub fn connect(&self, id: &str, send: Sender) -> McpPresenterConnection {
        let mut inner = lock(&self.shared.inner);
        let info = McpPresenterConnection {
            key: inner.key(),
            id: id.to_owned(),
            epoch: uuid(),
            nonce: uuid(),
        };
        inner.connections.insert(
            info.key,
            Connection {
                info: info.clone(),
                send,
                attachments: BTreeMap::new(),
            },
        );
        info
    }

    pub fn attach(&self, sender: &McpPresenterConnection, binding: &McpFormBinding) {
        let mut inner = lock(&self.shared.inner);
        if !inner.bindings.contains_key(&binding.id) {
            return;
        }
        if let Some(connection) = inner.connections.get_mut(&sender.key) {
            connection
                .attachments
                .entry(binding.id)
                .or_insert_with(|| Attachment {
                    id: uuid(),
                    epoch: uuid(),
                });
        }
    }

    pub fn connection_context(
        &self,
        sender: &McpPresenterConnection,
        session_id: &str,
    ) -> Option<McpPresenterContext> {
        let inner = lock(&self.shared.inner);
        let connection = inner.connections.get(&sender.key)?;
        let (key, binding) = inner.binding_for(session_id)?;
        let attachment = connection.attachments.get(&key)?;
        Some(McpPresenterContext {
            host_instance: self.shared.instance.clone(),
            connection_nonce: connection.info.nonce.clone(),
            binding_epoch: binding.epoch.clone(),
            attachment_id: attachment.id.clone(),
            attachment_epoch: attachment.epoch.clone(),
        })
    }

    /// Detach one binding, or (with `None`) the whole connection.
    pub fn detach(&self, sender: &McpPresenterConnection, binding: Option<u64>) {
        let mut revoked = Revoked::default();
        {
            let mut inner = lock(&self.shared.inner);
            let Some(connection) = inner.connections.get_mut(&sender.key) else {
                return;
            };
            let detached: Vec<u64> = connection
                .attachments
                .keys()
                .copied()
                .filter(|key| binding.is_none_or(|binding| binding == *key))
                .collect();
            for key in &detached {
                connection.attachments.remove(key);
            }
            for key in detached {
                let owned = inner
                    .bindings
                    .get(&key)
                    .and_then(|binding| binding.grant.clone())
                    .filter(|grant| grant.connection == sender.key);
                if let Some(grant) = owned {
                    Shared::revoke(&mut inner, &grant.registration, false, &mut revoked);
                }
            }
            if binding.is_none() {
                inner.connections.remove(&sender.key);
            }
        }
        revoked.run();
    }

    pub fn withdraw(&self, sender: &McpPresenterConnection) {
        let mut revoked = Revoked::default();
        {
            let mut inner = lock(&self.shared.inner);
            let Some(connection) = inner.connections.get(&sender.key) else {
                return;
            };
            let attached: Vec<u64> = connection.attachments.keys().copied().collect();
            for key in attached {
                let owned = inner
                    .bindings
                    .get(&key)
                    .and_then(|binding| binding.grant.clone())
                    .filter(|grant| grant.connection == sender.key);
                if let Some(grant) = owned {
                    Shared::revoke(&mut inner, &grant.registration, false, &mut revoked);
                }
            }
        }
        revoked.run();
    }

    fn dispose_binding(&self, key: u64) {
        let mut revoked = Revoked::default();
        {
            let mut inner = lock(&self.shared.inner);
            let Some(binding) = inner.bindings.get(&key) else {
                return;
            };
            revoked.bindings.push(binding.lifetime.clone());
            if let Some(grant) = binding.grant.clone() {
                Shared::revoke(&mut inner, &grant.registration, true, &mut revoked);
            }
            if let Some(binding) = inner.bindings.remove(&key)
                && inner.sessions.get(&binding.session_id) == Some(&key)
            {
                inner.sessions.remove(&binding.session_id);
            }
            for connection in inner.connections.values_mut() {
                connection.attachments.remove(&key);
            }
        }
        revoked.run();
    }

    pub fn bind(&self, session_id: &str, durable_id: &str, admit: Admit) -> Arc<McpFormBinding> {
        let previous = lock(&self.shared.inner).sessions.get(session_id).copied();
        if let Some(previous) = previous {
            self.dispose_binding(previous);
        }
        let lifetime = AbortController::new();
        let (key, epoch) = {
            let mut inner = lock(&self.shared.inner);
            let key = inner.key();
            let epoch = uuid();
            inner.bindings.insert(
                key,
                Binding {
                    session_id: session_id.to_owned(),
                    durable_id: durable_id.to_owned(),
                    epoch: epoch.clone(),
                    lifetime: lifetime.clone(),
                    admit,
                    listeners: BTreeMap::new(),
                    grant: None,
                },
            );
            inner.sessions.insert(session_id.to_owned(), key);
            (key, epoch)
        };
        let weak = Arc::downgrade(&self.shared);
        let (version, subscribe, capture, dispose) =
            (weak.clone(), weak.clone(), weak.clone(), weak);
        Arc::new(McpFormBinding {
            id: key,
            epoch,
            signal: lifetime.signal(),
            version: Some(Box::new(move || {
                let shared = version.upgrade()?;
                let inner = lock(&shared.inner);
                let grant = inner.bindings.get(&key)?.grant.clone()?;
                inner.current(&grant).then(|| grant.id.clone())
            })),
            subscribe: Some(Box::new(move |listener| {
                let Some(shared) = subscribe.upgrade() else {
                    return Box::new(|| {});
                };
                let id = {
                    let mut inner = lock(&shared.inner);
                    let id = inner.key();
                    if let Some(binding) = inner.bindings.get_mut(&key) {
                        binding.listeners.insert(id, listener);
                    }
                    id
                };
                let weak = Arc::downgrade(&shared);
                Box::new(move || {
                    if let Some(shared) = weak.upgrade()
                        && let Some(binding) = lock(&shared.inner).bindings.get_mut(&key)
                    {
                        binding.listeners.remove(&id);
                    }
                })
            })),
            capture: Box::new(move || {
                let shared = capture.upgrade()?;
                let grant = {
                    let inner = lock(&shared.inner);
                    let grant = inner.bindings.get(&key)?.grant.clone()?;
                    inner.current(&grant).then_some(grant)?
                };
                Some(Self { shared }.presentation(grant))
            }),
            dispose: Box::new(move || {
                if let Some(shared) = dispose.upgrade() {
                    Self { shared }.dispose_binding(key);
                }
            }),
        })
    }

    /// Called only after the protected bootstrap has pinned and authenticated its launch principal.
    pub fn enroll(&self, principal: Option<String>) -> McpPresenterOwner {
        McpPresenterOwner {
            shared: Arc::downgrade(&self.shared),
            principal: principal.unwrap_or_else(uuid),
            generation: uuid(),
        }
    }

    pub fn receive(&self, sender: &McpPresenterConnection, value: &Value) -> McpPresenterReply {
        let deny = McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED);
        let invalid = McpPresenterReply::Error("invalid_form_response");
        let record = value.as_object();
        let session = record
            .and_then(|record| record.get("sessionId"))
            .and_then(Value::as_str);
        // Check authority before parsing the private namespace, including invented dialog ids.
        let grant = {
            let inner = lock(&self.shared.inner);
            let grant = session
                .and_then(|session| inner.binding_for(session))
                .and_then(|(_, binding)| binding.grant.clone());
            match grant {
                Some(grant)
                    if inner.connections.contains_key(&sender.key)
                        && grant.connection == sender.key
                        && inner.current(&grant) =>
                {
                    grant
                }
                _ => return deny,
            }
        };
        let Some(record) = record else {
            return deny;
        };
        if record.get("grantId").and_then(Value::as_str) != Some(grant.id.as_str())
            || record.get("bindingEpoch").and_then(Value::as_str)
                != Some(grant.scope.binding_epoch.as_str())
            || record.get("attachmentEpoch").and_then(Value::as_str)
                != Some(grant.scope.attachment_epoch.as_str())
        {
            return deny;
        }
        let kind = record.get("type").and_then(Value::as_str);
        let mut allowed = vec![
            "type",
            "id",
            "sessionId",
            "bindingEpoch",
            "attachmentEpoch",
            "grantId",
            "dialogId",
            "dialogToken",
            "formRevision",
        ];
        if kind == Some("mcp_form_response") {
            allowed.extend(["action", "content"]);
        }
        if record.keys().any(|key| !allowed.contains(&key.as_str())) {
            return invalid;
        }
        if kind == Some("mcp_form_sync") {
            let first = lock(&grant.dialogs)
                .first()
                .map(|(_, dialog)| Arc::clone(dialog));
            return McpPresenterReply::Ok(first.and_then(|dialog| dialog.sync()));
        }
        let (Some(dialog_id), Some(token), Some(revision)) = (
            record.get("dialogId").and_then(Value::as_str),
            record.get("dialogToken").and_then(Value::as_str),
            record.get("formRevision").and_then(Value::as_str),
        ) else {
            return invalid;
        };
        let dialog = lock(&grant.dialogs)
            .iter()
            .find(|(id, _)| id == dialog_id)
            .map(|(_, dialog)| Arc::clone(dialog));
        let Some(dialog) = dialog else {
            return McpPresenterReply::Error("form_stale");
        };
        if !dialog.matches(token, revision) {
            return invalid;
        }
        match kind {
            Some("mcp_form_presented") => dialog.presented(),
            Some("mcp_form_response") => match record.get("action").and_then(Value::as_str) {
                Some(action) => dialog.respond(action, record.get("content")),
                None => invalid,
            },
            _ => invalid,
        }
    }

    fn current(&self, grant: &Arc<Grant>) -> bool {
        lock(&self.shared.inner).current(grant)
    }

    fn presentation(&self, grant: Arc<Grant>) -> McpFormPresentation {
        let (ready, present) = (self.clone(), self.clone());
        let (ready_grant, present_grant) = (Arc::clone(&grant), Arc::clone(&grant));
        McpFormPresentation {
            signal: grant.lifetime.signal(),
            ready: Arc::new(move || ready.current(&ready_grant)),
            present: Arc::new(move |input| {
                let (authority, grant) = (present.clone(), Arc::clone(&present_grant));
                Box::pin(async move { authority.present(grant, input).await })
            }),
            synchronize: None,
        }
    }

    async fn present(&self, grant: Arc<Grant>, input: McpFormInput) -> McpFormOutcome {
        let schema = &input.wire.schema;
        let oversized = serde_json::json!({"message":input.wire.message,"schema":schema})
            .to_string()
            .len()
            > 512 * 1024;
        if !self.current(&grant)
            || !crate::mcp_form_schema::validate_mrtr_form(schema)
            || schema["properties"]
                .as_object()
                .is_some_and(|properties| properties.len() > 64)
            || oversized
        {
            return McpFormOutcome::processing_failure();
        }
        let (binding_signal, admit, session_id, binding_epoch, connection) = {
            let inner = lock(&self.shared.inner);
            let Some(binding) = inner.bindings.get(&grant.binding) else {
                return McpFormOutcome::processing_failure();
            };
            let Some(connection) = inner.connections.get(&grant.connection) else {
                return McpFormOutcome::processing_failure();
            };
            (
                binding.lifetime.signal(),
                Arc::clone(&binding.admit),
                binding.session_id.clone(),
                binding.epoch.clone(),
                Arc::clone(&connection.send),
            )
        };
        let clock = Arc::clone(&self.shared.clock);
        let deadline = clock.now() + input.wire.deadline_ms;
        let combined = AbortController::new();
        let sources: [AbortSignal; 3] = [
            input.operation_signal.clone(),
            input.presentation_signal.clone(),
            grant.lifetime.signal(),
        ];
        let listeners: Vec<_> = sources
            .iter()
            .map(|signal| {
                let combined = combined.clone();
                let id = signal.add_abort_listener(move |_| combined.abort(None));
                if signal.aborted() {
                    combined.abort(None);
                }
                (signal.clone(), id)
            })
            .collect();
        let expired = combined.clone();
        let timer = clock.schedule(
            Box::new(move || expired.abort(None)),
            input.wire.deadline_ms.max(0.),
        );
        let signal = combined.signal();
        let queued = async {
            tokio::select! {
                biased;
                guard = grant.queue.lock() => {
                    if signal.aborted() { return Err(()); }
                    Ok(guard)
                }
                () = signal.cancelled() => Err(()),
            }
        }
        .await;
        let outcome = match queued {
            Err(()) => {
                if input.operation_stale() || binding_signal.aborted() {
                    McpFormOutcome::operation_aborted()
                } else {
                    McpFormOutcome::processing_failure()
                }
            }
            Ok(_guard) => {
                clock.cancel(timer);
                if !self.current(&grant) {
                    McpFormOutcome::processing_failure()
                } else {
                    let dialog_id = uuid();
                    let mut dialog_input = input.clone();
                    dialog_input.presentation_signal = signal.clone();
                    dialog_input.wire.deadline_ms = (deadline - clock.now()).max(0.);
                    let (current, remove) = (self.clone(), Arc::clone(&grant));
                    let (current_grant, removed) = (Arc::clone(&grant), dialog_id.clone());
                    let (dialog, result) = McpFormDialog::new(
                        dialog_input,
                        McpFormDialogContext {
                            clock: Arc::clone(&clock),
                            envelope: McpFormEnvelope {
                                session_id,
                                binding_epoch,
                                attachment_epoch: grant.scope.attachment_epoch.clone(),
                                grant_id: grant.id.clone(),
                                dialog_id: dialog_id.clone(),
                                form_revision: uuid(),
                            },
                            current: Box::new(move || current.current(&current_grant)),
                            remove: Box::new(move || {
                                lock(&remove.dialogs).retain(|(id, _)| *id != removed);
                            }),
                            admit,
                            send: connection,
                        },
                    );
                    lock(&grant.dialogs).push((dialog_id, Arc::clone(&dialog)));
                    dialog.start();
                    result
                        .await
                        .unwrap_or_else(|_| McpFormOutcome::processing_failure())
                }
            }
        };
        clock.cancel(timer);
        for (source, id) in listeners {
            source.remove_abort_listener(id);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_form_dialog::tests::{ManualClock, input};
    use crate::mcp_form_types::McpFormOutcomeKind;
    use serde_json::json;

    struct Fixture {
        clock: Arc<ManualClock>,
        authority: McpPresenterAuthority,
        owner: McpPresenterOwner,
        registration: String,
        binding: Arc<McpFormBinding>,
        a: McpPresenterConnection,
        b: McpPresenterConnection,
        frames: Arc<Mutex<Vec<Value>>>,
        /// Every `mcp_form_request` the presenter connection receives, as it is sent.
        requests: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Value>>,
        other: Arc<Mutex<Vec<Value>>>,
        admitted: Arc<std::sync::atomic::AtomicUsize>,
        operation: AbortController,
    }
    fn fixture() -> Fixture {
        let clock = Arc::new(ManualClock::default());
        let authority = McpPresenterAuthority::new(Some(clock.clone()));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let other = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&frames);
        let (requested, requests) = tokio::sync::mpsc::unbounded_channel();
        let a = authority.connect(
            "a",
            Arc::new(move |frame| {
                if frame["type"] == "mcp_form_request" {
                    lock(&sink).push(frame.clone());
                    let _ = requested.send(frame.clone());
                }
                true
            }),
        );
        let sink = Arc::clone(&other);
        let b = authority.connect(
            "b",
            Arc::new(move |frame| {
                lock(&sink).push(frame.clone());
                true
            }),
        );
        let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&admitted);
        let binding = authority.bind(
            "session",
            "durable",
            Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        authority.attach(&a, &binding);
        authority.attach(&b, &binding);
        // This is the protected launch-owner seam, never an RPC declaration.
        let owner = authority.enroll(None);
        let registration = owner
            .register(&binding, &a, McpPresenterKind::Desktop)
            .unwrap();
        owner.acquire(&registration).unwrap();
        Fixture {
            clock,
            authority,
            owner,
            registration,
            binding,
            a,
            b,
            frames,
            requests: tokio::sync::Mutex::new(requests),
            other,
            admitted,
            operation: AbortController::new(),
        }
    }
    impl Fixture {
        fn present(&self) -> futures::future::BoxFuture<'static, McpFormOutcome> {
            let presentation = self.binding.capture().unwrap();
            (presentation.present)(input(&self.operation))
        }
        fn pending(&self) -> tokio::task::JoinHandle<McpFormOutcome> {
            tokio::spawn(self.present())
        }
        /// Awaits the next `mcp_form_request` the presenter receives.
        async fn requested(&self) -> Value {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.requests.lock().await.recv(),
            )
            .await
            .unwrap()
            .unwrap()
        }
        fn frame(&self) -> Value {
            lock(&self.frames).last().cloned().unwrap()
        }
        fn envelope(&self) -> Value {
            let frame = self.frame();
            let mut envelope = json!({});
            for key in [
                "sessionId",
                "bindingEpoch",
                "attachmentEpoch",
                "grantId",
                "dialogId",
                "dialogToken",
                "formRevision",
            ] {
                envelope[key] = frame[key].clone();
            }
            envelope
        }
        fn action(&self, content: Value) -> Value {
            let mut action = self.envelope();
            action["type"] = "mcp_form_response".into();
            action["action"] = "accept".into();
            action["content"] = content;
            action
        }
        fn presented(&self) -> McpPresenterReply {
            let mut presented = self.envelope();
            presented["type"] = "mcp_form_presented".into();
            self.authority.receive(&self.a, &presented)
        }
        fn admitted(&self) -> usize {
            self.admitted.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    #[tokio::test]
    async fn settles_queued_forms_on_each_cause_without_displaying_them() {
        for cause in ["operation", "presenter", "binding", "timeout"] {
            let f = fixture();
            let first = f.pending();
            f.requested().await;
            // The second form parks on the grant's queue behind the displayed one.
            let mut second = f.present();
            assert!(futures::poll!(&mut second).is_pending());
            assert_eq!(lock(&f.frames).len(), 1);
            match cause {
                "operation" => f.operation.abort(None),
                "presenter" => f.authority.detach(&f.a, None),
                "binding" => f.binding.dispose(),
                _ => f.clock.expire(100.),
            }
            let expected = if matches!(cause, "operation" | "binding") {
                McpFormOutcomeKind::OperationAborted
            } else {
                McpFormOutcomeKind::ProcessingFailure
            };
            for outcome in [first.await.unwrap(), second.await] {
                assert_eq!(outcome.kind, expected, "{cause}");
                assert!(!outcome.human_admission());
            }
            assert_eq!(lock(&f.frames).len(), 1);
            assert_eq!(f.admitted(), 0);
            assert_eq!(f.clock.size(), 0, "{cause}");
            f.authority.dispose();
        }
    }

    #[tokio::test]
    async fn rejects_a_second_registration_without_an_unowned_grant() {
        let f = fixture();
        assert_eq!(
            f.owner
                .register(&f.binding, &f.a, McpPresenterKind::Desktop),
            Err(PRESENTER_NOT_AUTHORIZED)
        );
        assert!(
            f.binding
                .capture()
                .is_some_and(|presentation| (presentation.ready)())
        );
        f.owner.revoke(&f.registration);
        assert!(f.binding.capture().is_none());
    }

    #[tokio::test]
    async fn private_sync_expires_the_entry_even_when_its_timer_is_delayed() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        let mut sync = f.envelope();
        sync["type"] = "mcp_form_sync".into();
        *lock(&f.clock.now) = 100.;
        assert_eq!(
            f.authority.receive(&f.a, &sync),
            McpPresenterReply::Ok(None)
        );
        let outcome = result.await.unwrap();
        assert_eq!(outcome.kind, McpFormOutcomeKind::ProcessingFailure);
        assert_eq!(f.admitted(), 0);
    }

    #[tokio::test]
    async fn a_same_session_peer_cannot_read_or_answer_even_with_a_copied_token() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        assert!(lock(&f.other).is_empty());
        let answer = f.action(json!({"value":"answer-canary"}));
        assert_eq!(
            f.authority.receive(&f.b, &answer),
            McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED)
        );
        let mut invented = answer.clone();
        invented["dialogId"] = "invented".into();
        assert_eq!(
            f.authority.receive(&f.b, &invented),
            McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED)
        );
        assert_eq!(f.admitted(), 0);
        f.presented();
        assert_eq!(
            f.authority.receive(&f.a, &answer),
            McpPresenterReply::Ok(None)
        );
        let outcome = result.await.unwrap();
        assert_eq!(
            outcome.kind,
            McpFormOutcomeKind::Result(
                json!({"action":"accept","content":{"value":"answer-canary"}})
            )
        );
        assert!(outcome.human_admission());
        assert_eq!(f.admitted(), 1);
        assert_eq!(
            f.authority.receive(&f.a, &answer),
            McpPresenterReply::Error("form_stale")
        );
        assert_eq!(f.admitted(), 1);
    }

    #[tokio::test]
    async fn a_presented_revoked_holder_cancels_its_retained_entry() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        f.presented();
        let answer = f.action(json!({"value":"x"}));
        f.owner.revoke(&f.registration);
        let outcome = result.await.unwrap();
        assert_eq!(
            outcome.kind,
            McpFormOutcomeKind::Result(json!({"action":"cancel"}))
        );
        assert!(!outcome.human_admission());
        assert!(f.binding.capture().is_none());
        assert_eq!(f.admitted(), 0);
        assert!(lock(&f.other).is_empty());
        assert_eq!(
            f.authority.receive(&f.a, &answer),
            McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED)
        );
    }

    #[tokio::test]
    async fn binding_replacement_fences_a_response_queued_before_dispatch() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        f.presented();
        let queued = f.action(json!({"value":"x"}));
        f.binding.dispose();
        let _replacement = f.authority.bind("session", "replacement", Arc::new(|| {}));
        assert_eq!(
            f.authority.receive(&f.a, &queued),
            McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED)
        );
        assert_eq!(
            result.await.unwrap().kind,
            McpFormOutcomeKind::OperationAborted
        );
        assert_eq!(f.admitted(), 0);
        assert_eq!(lock(&f.frames).len(), 1);
    }

    #[tokio::test]
    async fn invalid_submissions_never_claim_the_terminal_latch() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        assert_eq!(
            f.authority.receive(&f.a, &f.action(json!({"value":"x"}))),
            McpPresenterReply::Error("invalid_form_response")
        );
        f.presented();
        for content in [
            json!({}),
            json!({"value":false}),
            json!({"value":"ok","extra":"forbidden"}),
        ] {
            assert_eq!(
                f.authority.receive(&f.a, &f.action(content)),
                McpPresenterReply::Error("invalid_form_response")
            );
        }
        let mut forged = f.action(json!({"value":"x"}));
        forged["humanAdmission"] = true.into();
        assert_eq!(
            f.authority.receive(&f.a, &forged),
            McpPresenterReply::Error("invalid_form_response")
        );
        assert_eq!(f.admitted(), 0);
        f.authority.receive(&f.a, &f.action(json!({"value":"x"})));
        assert!(result.await.unwrap().human_admission());
    }

    #[tokio::test]
    async fn operation_abort_wins_over_later_presentation_loss() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        f.presented();
        f.operation.abort(None);
        f.owner.revoke(&f.registration);
        f.clock.expire(100.);
        assert_eq!(
            result.await.unwrap().kind,
            McpFormOutcomeKind::OperationAborted
        );
        assert_eq!(f.admitted(), 0);
    }

    #[tokio::test]
    async fn private_sync_is_same_grant_only_and_empty_after_settlement() {
        let f = fixture();
        let result = f.pending();
        f.requested().await;
        let mut sync = f.envelope();
        sync["type"] = "mcp_form_sync".into();
        assert_eq!(
            f.authority.receive(&f.b, &sync),
            McpPresenterReply::Error(PRESENTER_NOT_AUTHORIZED)
        );
        assert_eq!(
            f.authority.receive(&f.a, &sync),
            McpPresenterReply::Ok(Some(f.frame()))
        );
        f.presented();
        f.authority.receive(&f.a, &f.action(json!({"value":"x"})));
        result.await.unwrap();
        assert_eq!(
            f.authority.receive(&f.a, &sync),
            McpPresenterReply::Ok(None)
        );
    }
}
