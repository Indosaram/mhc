//! senpi `modes/rpc/compact-message-identity.ts`: one lane epoch's view of settled,
//! durable-entry-join, and live-call identities for delivery (RULING-120, RULING-121, RULING-122).

use maho_core::session_transcript_journal::TranscriptTransition;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_PENDING_JOINS: usize = 1024;
pub const MAX_EVICTED_JOINS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIdentity {
    pub message_id: String,
    pub terminal_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallIdentity {
    pub call_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LiveIdentity {
    Terminal(TerminalIdentity),
    Message {
        #[serde(rename = "messageId")]
        message_id: String,
    },
    OwningMessage {
        #[serde(rename = "owningMessageId")]
        owning_message_id: String,
    },
    OwnerUnknown {
        #[serde(rename = "ownerUnknown")]
        owner_unknown: bool,
    },
    Call(CallIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledEnd {
    pub joined: bool,
    pub identity: TerminalIdentity,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageIdentitiesStatus {
    pub owner_joins_evicted: u64,
    pub owner_joins_overflowed: u64,
}

struct HeldIdentity {
    record_type: String,
    sequence: u64,
    identity: LiveIdentity,
}

/// One lane epoch's view of message/join/call identities for live delivery.
#[derive(Default)]
pub struct MessageIdentities {
    joins: HashMap<String, String>,
    joins_order: VecDeque<String>,
    unvouched: HashSet<String>,
    unvouched_order: VecDeque<String>,
    overflowed: bool,
    evictions: u64,
    overflows: u64,
    held: Option<HeldIdentity>,
}

impl MessageIdentities {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.joins.clear();
        self.joins_order.clear();
        self.unvouched.clear();
        self.unvouched_order.clear();
        self.overflowed = false;
        self.held = None;
    }

    pub fn status(&self) -> MessageIdentitiesStatus {
        MessageIdentitiesStatus {
            owner_joins_evicted: self.evictions,
            owner_joins_overflowed: self.overflows,
        }
    }

    pub fn joined(&mut self, transition: &TranscriptTransition, owner: Option<&str>) {
        let TranscriptTransition::Append {
            id,
            owning_message_id,
            ..
        } = transition
        else {
            return;
        };
        let Some(owner) = owner else {
            if owning_message_id.is_some() {
                self.unvouch(id);
            }
            return;
        };
        if !self.joins.contains_key(id) {
            self.joins_order.push_back(id.clone());
        }
        self.joins.insert(id.clone(), owner.to_owned());
        while self.joins.len() > MAX_PENDING_JOINS {
            if let Some(oldest) = self.joins_order.pop_front() {
                if self.joins.remove(&oldest).is_some() {
                    self.evictions += 1;
                    self.unvouch(&oldest);
                }
            } else {
                break;
            }
        }
    }

    fn unvouch(&mut self, entry_id: &str) {
        if self.unvouched.insert(entry_id.to_owned()) {
            self.unvouched_order.push_back(entry_id.to_owned());
        }
        while self.unvouched.len() > MAX_EVICTED_JOINS {
            if let Some(forgotten) = self.unvouched_order.pop_front() {
                if self.unvouched.remove(&forgotten) {
                    self.overflows += 1;
                    if !self.overflowed {
                        self.overflowed = true;
                        eprintln!(
                            "senpi rpc acceptance: unvouched-owner ring overflowed ({MAX_EVICTED_JOINS}; evicted or never-joined owned entries); unmatched entries carry ownerUnknown until the epoch ends"
                        );
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Every acceptance starts with nothing held, so a refused record never inherits its predecessor's ids.
    pub fn clear(&mut self) {
        self.held = None;
    }

    pub fn hold(
        &mut self,
        record_type: &str,
        sequence: u64,
        record: &Value,
        recorded: Option<LiveIdentity>,
    ) {
        let identity = recorded.or_else(|| {
            if record_type == "entry_appended" {
                self.take_entry_owner(record)
            } else {
                None
            }
        });
        if let Some(identity) = identity {
            self.held = Some(HeldIdentity {
                record_type: record_type.to_owned(),
                sequence,
                identity,
            });
        }
    }

    pub fn of(&self, record_type: Option<&str>, sequence: u64) -> Option<&LiveIdentity> {
        let held = self.held.as_ref()?;
        if Some(held.record_type.as_str()) == record_type && held.sequence == sequence {
            Some(&held.identity)
        } else {
            None
        }
    }

    fn take_entry_owner(&mut self, record: &Value) -> Option<LiveIdentity> {
        let entry_id = record
            .get("entry")
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)?;
        if let Some(owning_message_id) = self.joins.remove(entry_id) {
            return Some(LiveIdentity::OwningMessage { owning_message_id });
        }
        let unknown = self.unvouched.remove(entry_id) || self.overflowed;
        unknown.then_some(LiveIdentity::OwnerUnknown {
            owner_unknown: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn append(id: &str, owner: Option<&str>) -> TranscriptTransition {
        TranscriptTransition::Append {
            id: id.into(),
            entry: None,
            persistence: None,
            owning_message_id: owner.map(Into::into),
            recovery_epoch: Some("e1".into()),
        }
    }

    #[test]
    fn resolves_joined_entry_and_marks_unvouched_as_owner_unknown() {
        let mut ids = MessageIdentities::new();
        ids.joined(&append("e1", Some("m1")), Some("m1"));
        ids.joined(&append("e2", Some("m2")), None);

        ids.hold("entry_appended", 5, &json!({"entry":{"id":"e1"}}), None);
        assert_eq!(
            ids.of(Some("entry_appended"), 5),
            Some(&LiveIdentity::OwningMessage {
                owning_message_id: "m1".into()
            })
        );
        ids.clear();
        assert_eq!(ids.of(Some("entry_appended"), 5), None);

        ids.hold("entry_appended", 6, &json!({"entry":{"id":"e2"}}), None);
        assert_eq!(
            ids.of(Some("entry_appended"), 6),
            Some(&LiveIdentity::OwnerUnknown {
                owner_unknown: true
            })
        );
    }

    #[test]
    fn evicts_oldest_pending_join_into_unvouched_ring() {
        let mut ids = MessageIdentities::new();
        for i in 0..=MAX_PENDING_JOINS {
            let entry = format!("e-{i}");
            ids.joined(&append(&entry, Some("m")), Some("m"));
        }
        assert_eq!(ids.status().owner_joins_evicted, 1);

        ids.hold("entry_appended", 1, &json!({"entry":{"id":"e-0"}}), None);
        assert_eq!(
            ids.of(Some("entry_appended"), 1),
            Some(&LiveIdentity::OwnerUnknown {
                owner_unknown: true
            })
        );
    }
}
