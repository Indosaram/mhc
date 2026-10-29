//! In-memory delivery journal for lead mailbox reservations. Every message is recorded before its
//! injection is flushed, and is removed only after the recipient session durably contains the
//! envelope. This remains the handoff ledger paired with mailbox reconciliation when a process
//! exits mid-delivery.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use team_core::types::Message;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeadDeliveryJournalOptions {
    pub max_per_team: Option<usize>,
}

#[derive(Debug)]
pub struct LeadDeliveryJournal {
    max_per_team: usize,
    entries: Mutex<HashMap<String, Vec<Message>>>,
}

pub fn create_lead_delivery_journal(options: LeadDeliveryJournalOptions) -> LeadDeliveryJournal {
    LeadDeliveryJournal {
        max_per_team: options.max_per_team.unwrap_or(200),
        entries: Mutex::new(HashMap::new()),
    }
}

impl LeadDeliveryJournal {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<Message>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn record(&self, team_run_id: &str, message: Message) {
        let mut entries = self.lock();
        let list = entries.entry(team_run_id.to_string()).or_default();
        list.retain(|candidate| candidate.message_id != message.message_id);
        list.push(message);
        if list.len() > self.max_per_team {
            let excess = list.len() - self.max_per_team;
            list.drain(..excess);
        }
    }

    pub fn mark_reported(&self, team_run_id: &str, message_id: &str) {
        let mut entries = self.lock();
        let Some(list) = entries.get_mut(team_run_id) else {
            return;
        };
        if let Some(index) = list.iter().position(|candidate| candidate.message_id == message_id) {
            list.remove(index);
        }
    }

    pub fn take_oldest_unreported(&self, team_run_id: &str, from: Option<&str>) -> Option<Message> {
        let mut entries = self.lock();
        let list = entries.get_mut(team_run_id)?;
        let index = list
            .iter()
            .position(|candidate| from.is_none_or(|from| candidate.from == from))?;
        Some(list.remove(index))
    }

    pub fn drop_team(&self, team_run_id: &str) {
        self.lock().remove(team_run_id);
    }
}
