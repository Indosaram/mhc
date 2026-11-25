//! senpi `modes/rpc/compact-producer-stamp.ts`: the producer half of RULING-36a/40:
//! one lifetime ordinal per acceptance unit and each message's first canonical ordinal,
//! bound by the producer until its outer end.

use maho_core::session_recovery_identity::RecoveryCarrier;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProducerStamp {
    pub ordinal: i64,
    pub first_ordinal: Option<i64>,
}

#[derive(Debug, Clone)]
struct LastStamp {
    ordinal: i64,
    first_ordinal: Option<i64>,
    message_id: Option<String>,
}

impl Default for LastStamp {
    fn default() -> Self {
        Self {
            ordinal: -1,
            first_ordinal: None,
            message_id: None,
        }
    }
}

/// Lifetime ordinal tracker for canonical/legacy acceptance units produced in one session process.
#[derive(Debug, Default)]
pub struct CompactProducerStamp {
    next: i64,
    first: HashMap<String, i64>,
    last: LastStamp,
}

impl CompactProducerStamp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stamp(
        &mut self,
        carrier: Option<&RecoveryCarrier>,
        canonical_type: Option<&str>,
        legacy_type: Option<&str>,
    ) -> ProducerStamp {
        let ordinal = self.next;
        self.next += 1;
        self.last = LastStamp {
            ordinal,
            first_ordinal: None,
            message_id: None,
        };
        let Some(carrier) = carrier else {
            return ProducerStamp {
                ordinal,
                first_ordinal: None,
            };
        };
        let ends = canonical_type == Some("message_end") || legacy_type == Some("message_end");
        let first_ordinal = if canonical_type.is_some() {
            Some(
                self.first
                    .get(&carrier.message_id)
                    .copied()
                    .unwrap_or(ordinal),
            )
        } else {
            None
        };
        if ends {
            self.first.remove(&carrier.message_id);
        } else if let Some(first) = first_ordinal {
            self.first.insert(carrier.message_id.clone(), first);
        }
        self.last = LastStamp {
            ordinal,
            first_ordinal,
            message_id: Some(carrier.message_id.clone()),
        };
        ProducerStamp {
            ordinal,
            first_ordinal,
        }
    }

    /// The unit stamped last becomes the final commit frame of a transfer whose `prefix` fragment
    /// frames take the ordinals before it (RULING-64): its ordinal moves past them, and so does
    /// a first-ordinal binding it started itself.
    pub fn defer(&mut self, prefix: i64) -> ProducerStamp {
        let LastStamp {
            ordinal,
            first_ordinal,
            ref message_id,
        } = self.last;
        let moved = ordinal + prefix;
        self.next += prefix;
        let Some(first) = first_ordinal else {
            return ProducerStamp {
                ordinal: moved,
                first_ordinal: None,
            };
        };
        if first != ordinal {
            return ProducerStamp {
                ordinal: moved,
                first_ordinal: Some(first),
            };
        }
        if let Some(id) = message_id
            && self.first.get(id).copied() == Some(ordinal)
        {
            self.first.insert(id.clone(), moved);
        }
        ProducerStamp {
            ordinal: moved,
            first_ordinal: Some(moved),
        }
    }

    /// A replacement starts a new epoch, in which no message of the old one continues.
    pub fn forget(&mut self) {
        self.first.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carrier(id: &str) -> RecoveryCarrier {
        RecoveryCarrier {
            message_id: id.into(),
            epoch: "e1".into(),
            durable_entry_id: None,
        }
    }

    #[test]
    fn binds_first_ordinal_until_message_end() {
        let mut stamper = CompactProducerStamp::new();
        let c = carrier("m1");
        assert_eq!(
            stamper.stamp(Some(&c), Some("message_start"), None),
            ProducerStamp {
                ordinal: 0,
                first_ordinal: Some(0)
            }
        );
        assert_eq!(
            stamper.stamp(Some(&c), Some("message_update"), None),
            ProducerStamp {
                ordinal: 1,
                first_ordinal: Some(0)
            }
        );
        assert_eq!(
            stamper.stamp(Some(&c), None, Some("message_end")),
            ProducerStamp {
                ordinal: 2,
                first_ordinal: None
            }
        );
        // After end, a new start gets a fresh first_ordinal.
        assert_eq!(
            stamper.stamp(Some(&c), Some("message_start"), None),
            ProducerStamp {
                ordinal: 3,
                first_ordinal: Some(3)
            }
        );
    }

    #[test]
    fn defer_moves_ordinal_and_self_started_binding() {
        let mut stamper = CompactProducerStamp::new();
        let c = carrier("m1");
        stamper.stamp(Some(&c), Some("message_start"), None);
        assert_eq!(
            stamper.defer(3),
            ProducerStamp {
                ordinal: 3,
                first_ordinal: Some(3)
            }
        );
        assert_eq!(
            stamper.stamp(Some(&c), Some("message_update"), None),
            ProducerStamp {
                ordinal: 4,
                first_ordinal: Some(3)
            }
        );
    }

    #[test]
    fn forget_clears_open_bindings_without_resetting_ordinal() {
        let mut stamper = CompactProducerStamp::new();
        let c = carrier("m1");
        stamper.stamp(Some(&c), Some("message_start"), None);
        stamper.forget();
        assert_eq!(
            stamper.stamp(Some(&c), Some("message_update"), None),
            ProducerStamp {
                ordinal: 1,
                first_ordinal: Some(1)
            }
        );
    }
}
