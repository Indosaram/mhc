//! Lead mailbox poller: reserves unread lead messages, injects them into the lead session, and
//! commits each reservation only once the injected envelope is persisted in the lead session file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use team_core::team_mailbox::{
    InboxConsumerLeaseOptions, ack_messages, commit_delivery_reservation, is_message_consumed,
    list_unread_messages, release_delivery_reservation, reserve_message_for_delivery,
    with_inbox_consumer_lease,
};
use team_core::team_registry::{get_inbox_dir, resolve_base_dir};
use team_core::types::Message;

use crate::team::messaging::delivery_events::append_delivered_event;
use crate::team::messaging::lead_poller_types::{
    InvalidReservedLeadMessageError, LEAD_INJECTION_SOURCE, LeadInjection, LeadPollError, LeadPollFilter,
    LeadPollState, LeadPoller, LeadPollerDeps, PendingDelivery, PendingPhase,
};
use crate::team::messaging::message::build_peer_message_envelope;
use crate::team::messaging::session_marker_index::create_session_marker_index;
use crate::team::normalize::TEAM_LEAD_SENTINEL;

const DEAD_PID_LEASE_STALE_MS: i64 = 0;
const RESERVED_PREFIX: &str = ".delivering-";
const RESERVED_SUFFIX: &str = ".json";

/// Concrete lead poller created by [`create_lead_poller`].
pub struct TeamLeadPoller {
    deps: LeadPollerDeps,
    state: Arc<LeadPollState>,
    stopped: Arc<AtomicBool>,
}

pub fn create_lead_poller(deps: LeadPollerDeps) -> TeamLeadPoller {
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_flag = Arc::clone(&stopped);
    let state = LeadPollState {
        pending: Mutex::new(std::collections::HashMap::new()),
        is_stopped: Box::new(move || stopped_flag.load(Ordering::SeqCst)),
        marker_index: create_session_marker_index(None),
    };
    TeamLeadPoller {
        deps,
        state: Arc::new(state),
        stopped,
    }
}

impl LeadPoller for TeamLeadPoller {
    fn poll_once(&self, filter: Option<&LeadPollFilter>) -> Result<(), LeadPollError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut outcome: Result<(), LeadPollError> = Ok(());
        with_inbox_consumer_lease(
            &self.deps.team_run_id,
            TEAM_LEAD_SENTINEL,
            &self.deps.config,
            || -> team_core::Result<()> {
                outcome = self.poll_under_lease(filter);
                Ok(())
            },
            InboxConsumerLeaseOptions {
                stale_after_ms: DEAD_PID_LEASE_STALE_MS,
            },
        )?;
        outcome
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl TeamLeadPoller {
    fn poll_under_lease(&self, filter: Option<&LeadPollFilter>) -> Result<(), LeadPollError> {
        self.settle_pending()?;
        self.recover_reservations()?;
        let messages = list_unread_messages(&self.deps.team_run_id, TEAM_LEAD_SENTINEL, &self.deps.config)?;
        let from_filter = filter.and_then(|filter| filter.from.as_deref());
        for message in messages {
            if let Some(from) = from_filter
                && message.from != from
            {
                continue;
            }
            self.process_message(message)?;
        }
        Ok(())
    }

    fn lead_session_file(&self) -> Option<PathBuf> {
        self.deps.lead_session_file.as_ref().and_then(|session_file| session_file())
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, PendingDelivery>> {
        self.state.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn mark_delivered(&self, message: &Message) {
        if let Some(journal) = self.deps.delivery_journal.as_ref() {
            journal.mark_reported(&self.deps.team_run_id, &message.message_id);
        }
        append_delivered_event(&self.deps, message);
    }

    fn settle_pending(&self) -> Result<(), LeadPollError> {
        let ids: Vec<String> = self.lock_pending().keys().cloned().collect();
        for id in ids {
            let mut pending = self.lock_pending();
            let Some(delivery) = pending.get(&id) else {
                continue;
            };
            let release_when_missing = match delivery.phase {
                PendingPhase::AwaitingFlush => continue,
                PendingPhase::AwaitingPersistence => false,
                PendingPhase::Recovery => true,
            };
            let Some(session_file) = self.lead_session_file() else {
                continue;
            };
            let persisted = self
                .state
                .marker_index
                .contains(Some(&session_file), &delivery.message.message_id)?;
            if !persisted && !release_when_missing {
                continue;
            }
            if !persisted {
                release_delivery_reservation(&delivery.reservation)?;
                pending.remove(&id);
                continue;
            }
            commit_delivery_reservation(&delivery.reservation)?;
            if let Some(delivery) = pending.remove(&id) {
                drop(pending);
                self.mark_delivered(&delivery.message);
            }
        }
        Ok(())
    }

    fn process_message(&self, message: Message) -> Result<(), LeadPollError> {
        let team_run_id = &self.deps.team_run_id;
        let config = &self.deps.config;
        if is_message_consumed(team_run_id, TEAM_LEAD_SENTINEL, &message.message_id, config)? {
            ack_messages(team_run_id, TEAM_LEAD_SENTINEL, std::slice::from_ref(&message.message_id), config)?;
            return Ok(());
        }

        let Some(reservation) =
            reserve_message_for_delivery(team_run_id, TEAM_LEAD_SENTINEL, &message.message_id, config)?
        else {
            return Ok(());
        };
        if (self.state.is_stopped)() {
            release_delivery_reservation(&reservation)?;
            return Ok(());
        }
        if let Some(journal) = self.deps.delivery_journal.as_ref() {
            journal.record(team_run_id, message.clone());
        }

        let session_file = self.lead_session_file();
        if self
            .state
            .marker_index
            .contains(session_file.as_deref(), &message.message_id)?
        {
            commit_delivery_reservation(&reservation)?;
            self.mark_delivered(&message);
            return Ok(());
        }

        let message_id = message.message_id.clone();
        let content = build_peer_message_envelope(&message);
        self.lock_pending().insert(
            message_id.clone(),
            PendingDelivery {
                message,
                reservation,
                phase: PendingPhase::AwaitingFlush,
            },
        );

        let state = Arc::clone(&self.state);
        let failed_state = Arc::clone(&self.state);
        let failed_id = message_id.clone();
        let flushed_id = message_id.clone();
        self.deps.coordinator.enqueue(LeadInjection {
            key: format!("team-message:{message_id}"),
            source: LEAD_INJECTION_SOURCE,
            content,
            on_flushed: Some(Box::new(move || {
                let mut pending = state.pending.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(current) = pending.get_mut(&flushed_id)
                    && current.phase == PendingPhase::AwaitingFlush
                {
                    current.phase = PendingPhase::AwaitingPersistence;
                }
            })),
            on_delivery_failed: Some(Box::new(move |error| {
                let mut pending = failed_state.pending.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(delivery) = pending.get(&failed_id) else { return; };
                if delivery.phase != PendingPhase::AwaitingFlush { return; }
                match release_delivery_reservation(&delivery.reservation) {
                    Ok(()) => { pending.remove(&failed_id); }
                    Err(release_error) => {
                        utils::logger::log("senpi-task lead reservation release failed", Some(&serde_json::json!({ "messageId": failed_id, "deliveryError": error, "error": release_error.to_string() })));
                        if let Some(delivery) = pending.get_mut(&failed_id) { delivery.phase = PendingPhase::Recovery; }
                    }
                }
            })),
        });
        Ok(())
    }

    fn recover_reservations(&self) -> Result<(), LeadPollError> {
        let dir = self.inbox_dir()?;
        let read = match std::fs::read_dir(&dir) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        for entry in read {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.starts_with(RESERVED_PREFIX) && name.ends_with(RESERVED_SUFFIX) {
                entries.push(name);
            }
        }
        entries.sort();

        let team_run_id = &self.deps.team_run_id;
        let config = &self.deps.config;
        for entry in entries {
            let message = read_reserved_message(&dir.join(&entry))?;
            if self.lock_pending().contains_key(&message.message_id) {
                continue;
            }
            let Some(reservation) =
                reserve_message_for_delivery(team_run_id, TEAM_LEAD_SENTINEL, &message.message_id, config)?
            else {
                continue;
            };
            match self.lead_session_file() {
                None => {
                    self.lock_pending().insert(
                        message.message_id.clone(),
                        PendingDelivery {
                            message,
                            reservation,
                            phase: PendingPhase::Recovery,
                        },
                    );
                }
                Some(session_file) => {
                    if self
                        .state
                        .marker_index
                        .contains(Some(&session_file), &message.message_id)?
                    {
                        commit_delivery_reservation(&reservation)?;
                        self.mark_delivered(&message);
                    } else {
                        release_delivery_reservation(&reservation)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn inbox_dir(&self) -> team_core::Result<PathBuf> {
        get_inbox_dir(
            &resolve_base_dir(&self.deps.config),
            &self.deps.team_run_id,
            TEAM_LEAD_SENTINEL,
        )
    }
}

fn read_reserved_message(path: &Path) -> Result<Message, LeadPollError> {
    let bytes = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&bytes);
    let invalid = || InvalidReservedLeadMessageError::new(path.display().to_string());
    let value: Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
    Message::safe_parse(&value).map_err(|_| LeadPollError::from(invalid()))
}
