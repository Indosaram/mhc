//! Port of `team/member-extension/self-poller.ts`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::{Value, json};
use team_core::TeamCoreError;
use team_core::config::TeamModeConfig;
use team_core::team_mailbox::{
    DeliveryReservation, InboxConsumerLeaseOptions, ack_messages, commit_delivery_reservation,
    is_message_consumed, list_unread_messages, release_delivery_reservation,
    reserve_message_for_delivery, with_inbox_consumer_lease,
};
use team_core::team_registry::{get_inbox_dir, resolve_base_dir};
use team_core::types::Message;

use crate::team::member_extension::qa_inject_hold::QaAfterInjectHold;
use crate::team::member_extension::session_scan::{
    SessionAckScan, SessionJsonlSignature, is_missing_path, session_jsonl_signature,
};
use crate::team::messaging::message::build_peer_message_envelope;

pub use crate::team::member_extension::session_scan::session_jsonl_contains_message;

const DEAD_PID_LEASE_STALE_MS: i64 = 0;
const RESERVED_PREFIX: &str = ".delivering-";
const RESERVED_SUFFIX: &str = ".json";

/// Persisted task event emitted by the member extension (`{ type, payload }`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeamMessageEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: Value,
}

impl TeamMessageEvent {
    /// Builds a `{ type, payload: { message_id, from, to, kind } }` event for a team message.
    pub fn for_message(event_type: &str, message: &Message) -> Self {
        Self {
            event_type: event_type.to_string(),
            payload: json!({
                "message_id": message.message_id,
                "from": message.from,
                "to": message.to,
                "kind": message.kind.as_str(),
            }),
        }
    }
}

pub type InjectFn = Box<dyn Fn(&str) + Send + Sync>;
pub type AppendEventFn = Box<dyn Fn(TeamMessageEvent) + Send + Sync>;

pub struct MemberSelfPollerDeps {
    pub team_run_id: String,
    pub member_name: String,
    pub config: TeamModeConfig,
    pub session_dir: PathBuf,
    pub inject: InjectFn,
    pub append_event: Option<AppendEventFn>,
    pub after_inject: Option<QaAfterInjectHold>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberPollFilter {
    pub from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SelfPollerError {
    #[error(transparent)]
    TeamCore(#[from] TeamCoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Invalid reserved team message: {path}")]
    InvalidReservedMessage { path: String },
}

impl SelfPollerError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::TeamCore(error) => error.name(),
            Self::Io(_) => "Error",
            Self::InvalidReservedMessage { .. } => "InvalidReservedMessageError",
        }
    }
}

pub struct PendingDelivery {
    pub message: Message,
    pub reservation: DeliveryReservation,
}

pub struct MemberSelfPoller {
    deps: MemberSelfPollerDeps,
    pending: Mutex<Vec<(String, PendingDelivery)>>,
    /// Session JSONL signature of the last ack scan that missed, per pending message id.
    ack_misses: Mutex<HashMap<String, SessionJsonlSignature>>,
    /// Offset-based envelope index the repeated ack check reads incrementally.
    ack_scan: Mutex<SessionAckScan>,
    stopped: AtomicBool,
}

pub fn create_member_self_poller(deps: MemberSelfPollerDeps) -> MemberSelfPoller {
    MemberSelfPoller {
        deps,
        pending: Mutex::new(Vec::new()),
        ack_misses: Mutex::new(HashMap::new()),
        ack_scan: Mutex::new(SessionAckScan::default()),
        stopped: AtomicBool::new(false),
    }
}

impl MemberSelfPoller {
    pub fn poll_once(&self, filter: Option<&MemberPollFilter>) -> Result<(), SelfPollerError> {
        if self.is_stopped() {
            return Ok(());
        }
        if self.pending_lock().is_empty() && !self.inbox_has_unread() {
            return Ok(());
        }
        let from = filter.and_then(|filter| filter.from.clone());
        self.with_lease(|| {
            self.check_pending_under_lease()?;
            let messages =
                list_unread_messages(&self.deps.team_run_id, &self.deps.member_name, &self.deps.config)?;
            for message in messages {
                if let Some(from) = from.as_ref()
                    && &message.from != from
                {
                    continue;
                }
                self.process_message(message)?;
            }
            Ok(())
        })
    }

    pub fn check_pending_acks(&self) -> Result<(), SelfPollerError> {
        if self.is_stopped() {
            return Ok(());
        }
        if self.pending_lock().is_empty() || self.all_pending_acks_unchanged() {
            return Ok(());
        }
        self.with_lease(|| self.check_pending_under_lease())
    }

    pub fn recover_reservations(&self) -> Result<(), SelfPollerError> {
        if self.is_stopped() {
            return Ok(());
        }
        self.with_lease(|| recover_reservations(&self.deps))
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Message ids currently injected but not yet acked through the session JSONL.
    pub fn pending_message_ids(&self) -> Vec<String> {
        self.pending_lock().iter().map(|(id, _)| id.clone()).collect()
    }

    /// Lease-free pre-check: false only when the inbox provably holds no unread `*.json`.
    /// Any read error falls through to the leased path so errors surface as before.
    fn inbox_has_unread(&self) -> bool {
        let Ok(dir) = get_inbox_dir(
            &resolve_base_dir(&self.deps.config),
            &self.deps.team_run_id,
            &self.deps.member_name,
        ) else {
            return true;
        };
        let read_dir = match fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(error) if is_missing_path(&error) => return false,
            Err(_) => return true,
        };
        for entry in read_dir {
            let Ok(entry) = entry else {
                return true;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".json") && !name.starts_with('.') {
                return true;
            }
        }
        false
    }

    /// Lease-free pre-check: true only when every pending id already missed against the
    /// current session signature, so the leased scan would skip them all. An unreadable
    /// signature or any unrecorded id takes the leased path.
    fn all_pending_acks_unchanged(&self) -> bool {
        let Ok(signature) = session_jsonl_signature(&self.deps.session_dir) else {
            return false;
        };
        let misses = self.ack_misses.lock().unwrap_or_else(PoisonError::into_inner);
        let pending = self.pending_lock();
        !pending.is_empty() && pending.iter().all(|(id, _)| misses.get(id) == Some(&signature))
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn pending_lock(&self) -> MutexGuard<'_, Vec<(String, PendingDelivery)>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_lease<F>(&self, mut f: F) -> Result<(), SelfPollerError>
    where
        F: FnMut() -> Result<(), SelfPollerError>,
    {
        let mut outcome: Result<(), SelfPollerError> = Ok(());
        let options = InboxConsumerLeaseOptions {
            stale_after_ms: DEAD_PID_LEASE_STALE_MS,
        };
        let run = || -> team_core::Result<()> {
            outcome = f();
            Ok(())
        };
        with_inbox_consumer_lease(
            &self.deps.team_run_id,
            &self.deps.member_name,
            &self.deps.config,
            run,
            options,
        )?;
        outcome
    }

    fn restore_pending(&self, kept: Vec<(String, PendingDelivery)>) {
        let mut pending = self.pending_lock();
        let added = std::mem::take(&mut *pending);
        *pending = kept;
        pending.extend(added);
    }

    fn check_pending_under_lease(&self) -> Result<(), SelfPollerError> {
        let snapshot = std::mem::take(&mut *self.pending_lock());
        // Transcripts are append-only: an unchanged signature cannot hold an envelope a
        // previous scan missed. Signature errors fall back to scanning.
        let signature = if snapshot.is_empty() {
            None
        } else {
            session_jsonl_signature(&self.deps.session_dir).ok()
        };
        let mut misses = self.ack_misses.lock().unwrap_or_else(PoisonError::into_inner);
        let mut scan = self.ack_scan.lock().unwrap_or_else(PoisonError::into_inner);
        let mut scanned = false;
        let mut kept: Vec<(String, PendingDelivery)> = Vec::new();
        let mut iter = snapshot.into_iter();
        while let Some((id, delivery)) = iter.next() {
            if let Some(signature) = signature.as_ref()
                && misses.get(&id) == Some(signature)
            {
                kept.push((id, delivery));
                continue;
            }
            if !scanned {
                if let Err(error) = scan.refresh(&self.deps.session_dir) {
                    kept.push((id, delivery));
                    kept.extend(iter);
                    self.restore_pending(kept);
                    return Err(error.into());
                }
                scanned = true;
            }
            let contained = scan.contains(&delivery.message.message_id);
            if !contained {
                if let Some(signature) = signature.as_ref() {
                    misses.insert(id.clone(), signature.clone());
                }
                kept.push((id, delivery));
                continue;
            }
            misses.remove(&id);
            if let Err(error) = commit_delivery_reservation(&delivery.reservation) {
                kept.push((id, delivery));
                kept.extend(iter);
                self.restore_pending(kept);
                return Err(error.into());
            }
            append_delivered_event(&self.deps, &delivery.message);
        }
        self.restore_pending(kept);
        Ok(())
    }

    fn process_message(&self, message: Message) -> Result<(), SelfPollerError> {
        let deps = &self.deps;
        if is_message_consumed(&deps.team_run_id, &deps.member_name, &message.message_id, &deps.config)? {
            ack_messages(
                &deps.team_run_id,
                &deps.member_name,
                std::slice::from_ref(&message.message_id),
                &deps.config,
            )?;
            return Ok(());
        }

        let Some(reservation) = reserve_message_for_delivery(
            &deps.team_run_id,
            &deps.member_name,
            &message.message_id,
            &deps.config,
        )?
        else {
            return Ok(());
        };

        if self.is_stopped() {
            release_delivery_reservation(&reservation)?;
            return Ok(());
        }

        if session_jsonl_contains_message(&deps.session_dir, &message.message_id)? {
            commit_delivery_reservation(&reservation)?;
            append_delivered_event(deps, &message);
            return Ok(());
        }

        let envelope = build_peer_message_envelope(&message);
        let message_id = message.message_id.clone();
        self.ack_misses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&message_id);
        {
            let mut pending = self.pending_lock();
            let delivery = PendingDelivery {
                message: message.clone(),
                reservation,
            };
            match pending.iter_mut().find(|(id, _)| id == &message_id) {
                Some(slot) => slot.1 = delivery,
                None => pending.push((message_id, delivery)),
            }
        }
        (deps.inject)(&envelope);
        if let Some(after_inject) = deps.after_inject.as_ref() {
            after_inject(&message)?;
        }
        Ok(())
    }
}

fn recover_reservations(deps: &MemberSelfPollerDeps) -> Result<(), SelfPollerError> {
    let dir = inbox_dir(deps)?;
    let read_dir = match fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(error) if is_missing_path(&error) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut entries: Vec<String> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if is_missing_path(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(RESERVED_PREFIX) && name.ends_with(RESERVED_SUFFIX) {
            entries.push(name);
        }
    }
    entries.sort();

    for entry in &entries {
        let message = read_reserved_message(&dir.join(entry))?;
        let Some(reservation) = reserve_message_for_delivery(
            &deps.team_run_id,
            &deps.member_name,
            &message.message_id,
            &deps.config,
        )?
        else {
            continue;
        };
        if session_jsonl_contains_message(&deps.session_dir, &message.message_id)? {
            commit_delivery_reservation(&reservation)?;
            append_delivered_event(deps, &message);
        } else {
            release_delivery_reservation(&reservation)?;
        }
    }
    Ok(())
}

fn read_reserved_message(path: &Path) -> Result<Message, SelfPollerError> {
    let invalid = || SelfPollerError::InvalidReservedMessage {
        path: path.display().to_string(),
    };
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let parsed: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
    Message::safe_parse(&parsed).map_err(|_| invalid())
}

fn inbox_dir(deps: &MemberSelfPollerDeps) -> Result<PathBuf, SelfPollerError> {
    let base_dir = deps.config.base_dir.as_deref().unwrap_or("");
    Ok(get_inbox_dir(Path::new(base_dir), &deps.team_run_id, &deps.member_name)?)
}

fn append_delivered_event(deps: &MemberSelfPollerDeps, message: &Message) {
    if let Some(append_event) = deps.append_event.as_ref() {
        append_event(TeamMessageEvent::for_message("team_message_delivered", message));
    }
}
