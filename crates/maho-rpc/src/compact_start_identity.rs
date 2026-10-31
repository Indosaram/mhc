//! senpi `modes/rpc/compact-start-identity.ts`: R1 uses the producer's lifetime ordinal and the S05
//! opaque nonce, never a receipt-time identity.
use crate::compact_transition::{BlockStart, CompactRejection, CompactTransitionError};
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
pub struct CompactStartIdentity {
    current: Option<String>,
    high_water: i64,
    live: HashMap<String, (String, i64)>,
    /// Exactly the last 64 retirements, not an unbounded historical nonce catalog.
    // After 64 newer epochs an evicted epoch is indistinguishable from a new one; re-accepting it makes
    // it current and retires the live current epoch (takeover). Acceptable only because S05 nonces are
    // random and terminated-worker channels are closed.
    retired: VecDeque<String>,
}

impl CompactStartIdentity {
    pub fn new() -> Self {
        Self {
            high_water: -1,
            ..Self::default()
        }
    }

    pub fn validate(&self, start: &BlockStart) -> Result<(), CompactTransitionError> {
        let binding = self.live.get(&start.message_id);
        if start.epoch.is_empty()
            || start.ordinal < 0
            || start.first_ordinal < 0
            || start.first_ordinal > start.ordinal
            || binding.is_some_and(|(epoch, first)| {
                *epoch != start.epoch || *first != start.first_ordinal
            })
        {
            return Err(CompactTransitionError::new(
                CompactRejection::InvalidStartIdentity,
                "valid epoch, ordinal and stable first ordinal required",
            ));
        }
        let current = self.current.as_deref() == Some(start.epoch.as_str());
        if self.retired.contains(&start.epoch)
            || (current
                && (start.ordinal <= self.high_water
                    || (binding.is_none() && start.first_ordinal <= self.high_water)))
        {
            return Err(CompactTransitionError::new(
                CompactRejection::DuplicateOrStaleStart,
                "retired epoch or stale ordinal; bootstrap a new producer epoch before restarting its counter",
            ));
        }
        if binding.is_none() && start.first_ordinal != start.ordinal {
            return Err(CompactTransitionError::new(
                CompactRejection::InvalidStartIdentity,
                "a new message must bind its own first start",
            ));
        }
        Ok(())
    }

    /// Called only after every semantic validation and the root publication succeeded.
    pub fn commit(&mut self, start: &BlockStart) {
        if self.current.as_deref() != Some(start.epoch.as_str()) {
            if let Some(current) = self.current.take() {
                self.retired.push_back(current);
            }
            if self.retired.len() > 64 {
                self.retired.pop_front();
            }
            self.current = Some(start.epoch.clone());
        }
        self.high_water = start.ordinal;
        self.live.insert(
            start.message_id.clone(),
            (start.epoch.clone(), start.first_ordinal),
        );
    }

    pub fn release(&mut self, message_id: &str) {
        self.live.remove(message_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_transition::CompactBlockKind;

    fn start(epoch: &str, ordinal: i64, message_id: &str) -> BlockStart {
        BlockStart {
            message_id: message_id.into(),
            content_index: 0,
            epoch: epoch.into(),
            ordinal,
            first_ordinal: ordinal,
            kind: CompactBlockKind::Text,
            seed: "body".into(),
            covered_code_units: 0,
        }
    }

    fn accept(
        identity: &mut CompactStartIdentity,
        start: &BlockStart,
    ) -> Result<(), CompactRejection> {
        identity.validate(start).map_err(|error| error.reason)?;
        identity.commit(start);
        Ok(())
    }

    // senpi 4272-compact-start-identity: retired epochs, duplicates and same-epoch resets are stale.
    #[test]
    fn rejects_retired_duplicate_and_reset_starts() {
        let cases: [(&[(&str, i64)], (&str, i64)); 3] = [
            (&[("A", 0), ("B", 0)], ("A", 1)),
            (&[("A", 0)], ("A", 0)),
            (&[("A", 7)], ("A", 0)),
        ];
        for (accepted, rejected) in cases {
            let mut identity = CompactStartIdentity::new();
            for (epoch, ordinal) in accepted {
                accept(
                    &mut identity,
                    &start(epoch, *ordinal, &format!("{epoch}-{ordinal}")),
                )
                .expect("accepted");
            }
            assert_eq!(
                accept(
                    &mut identity,
                    &start(rejected.0, rejected.1, "different-key")
                ),
                Err(CompactRejection::DuplicateOrStaleStart)
            );
        }
    }

    #[test]
    fn accepts_never_seen_epochs_and_binds_first_ordinals() {
        let mut identity = CompactStartIdentity::new();
        for (epoch, ordinal) in [("A", 4), ("B", 0), ("C", 0)] {
            accept(&mut identity, &start(epoch, ordinal, epoch)).expect("accepted");
        }
        let mut next = start("C", 2, "C");
        next.first_ordinal = 0;
        accept(&mut identity, &next).expect("same message, later block");
        let mut moved = start("C", 3, "C");
        moved.first_ordinal = 1;
        assert_eq!(
            accept(&mut identity, &moved),
            Err(CompactRejection::InvalidStartIdentity)
        );
        let mut unbound = start("C", 5, "new");
        unbound.first_ordinal = 4;
        assert_eq!(
            accept(&mut identity, &unbound),
            Err(CompactRejection::InvalidStartIdentity)
        );
    }
}
