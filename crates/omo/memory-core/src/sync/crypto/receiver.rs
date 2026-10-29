use super::{
    entry::SyncEntry,
    envelope::envelope_refusal,
    vendor::sync_format::{
        fold::{ReceivedChain, ReceiverRefusal, fold_refusal},
        receive::receive_keyring_chain,
    },
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The merged omo-service receiver (vendored sync-format), run over the client's entries so the
/// client refuses exactly what the server's reference receiver refuses (sync-design 8.2): canonical
/// member and wrap order, recovery-join author and enrolment, one veto per join, batch rows without
/// bodies, monotonic sequence, and every fork-resolution's branches and evidence proof.
///
/// The entries cross as their wire JSON: the receiver reads the transport shape and validates
/// every field it uses itself.
pub fn wire(entries: &[&SyncEntry]) -> Vec<Value> {
    entries.iter().map(|entry| entry.wire()).collect()
}

/// A receiver refusal, or the receiver refusing an already-verified history entry instead of this one.
pub type ReceiverCheck = &'static str;

/// The receiver's verdict for one entry of the pull a ReceiverPass was built for.
pub type ReceiverPass = Box<dyn Fn(&SyncEntry) -> Option<ReceiverCheck>>;

const RECEIVER_SEQ_MISMATCH: ReceiverCheck = "receiver_seq_mismatch";
const SEQ_NOT_INCREASING: ReceiverRefusal = "seq_not_increasing";

fn first_refusal_at(
    received: &ReceivedChain,
    entry: &SyncEntry,
    ours: &BTreeSet<u64>,
) -> Option<ReceiverCheck> {
    let Err((seq, reason)) = received else {
        return None;
    };
    if *seq > entry.seq() {
        return None;
    }
    Some(if *seq == entry.seq() && ours.contains(seq) {
        reason
    } else {
        RECEIVER_SEQ_MISMATCH
    })
}

/// Entries are identified by address, as the TS identifies them by object identity: the caller keeps
/// the slice `receiver_pass` was built from alive and unmoved while it consults the pass.
fn identity(entry: &SyncEntry) -> usize {
    std::ptr::from_ref(entry) as usize
}

/// The merged receiver run ONCE over the already-verified keyring `history` and the keyring entries of
/// `entries`, in order, up to the first malformed one or the first whose seq does not increase. Every seq in
/// that walk names exactly one entry, so the receiver's first refusal (reported by seq) names one entry too:
/// entries before it pass, it gets the reason, and nothing after it is ever reached. That equals running each
/// entry over the history plus every earlier entry of the pull, because the fold judges a record only by the
/// records before it; so a pull costs one pass, not one pass per entry. An entry whose seq does not increase
/// gets the receiver's own `seq_not_increasing`, and nothing after it joins the walk.
/// A batch entry's receiver checks read only its own fields and the sequence order, so it is folded alone
/// and its seq is compared with the last keyring seq before it. An entry the pass was not built for is refused.
pub fn receiver_pass(history: &[SyncEntry], entries: &[SyncEntry]) -> ReceiverPass {
    let mut keyring: Vec<&SyncEntry> = Vec::new();
    let mut last_seq_before: BTreeMap<usize, u64> = BTreeMap::new();
    let mut replayed: Option<usize> = None;
    let mut last_seq = history.iter().map(SyncEntry::seq).max().unwrap_or(0);
    for entry in entries {
        if envelope_refusal(entry).is_some() {
            break;
        }
        if matches!(entry, SyncEntry::Keyring(_)) {
            if entry.seq() <= last_seq {
                replayed = Some(identity(entry));
                break;
            }
            keyring.push(entry);
            last_seq = last_seq.max(entry.seq());
        } else {
            last_seq_before.insert(identity(entry), last_seq);
        }
    }
    let mut list: Vec<Value> = history.iter().map(SyncEntry::wire).collect();
    list.extend(wire(&keyring));
    let has_fork = keyring.iter().any(|entry| {
        entry
            .body()
            .is_some_and(|body| body["kind"] == "fork-resolution")
    });
    let received = if has_fork {
        receive_keyring_chain(&list)
    } else {
        fold_refusal(&list)
    };
    let ours: BTreeSet<u64> = keyring.iter().map(|entry| entry.seq()).collect();
    let in_pull: BTreeSet<usize> = keyring.iter().map(|entry| identity(entry)).collect();
    Box::new(move |entry| {
        let id = identity(entry);
        if in_pull.contains(&id) {
            return first_refusal_at(&received, entry, &ours);
        }
        if replayed == Some(id) {
            return Some(if received.is_ok() {
                SEQ_NOT_INCREASING
            } else {
                RECEIVER_SEQ_MISMATCH
            });
        }
        let Some(before) = last_seq_before.get(&id) else {
            return Some(RECEIVER_SEQ_MISMATCH);
        };
        if entry.seq() <= *before {
            return Some(SEQ_NOT_INCREASING);
        }
        first_refusal_at(
            &fold_refusal(&wire(&[entry])),
            entry,
            &BTreeSet::from([entry.seq()]),
        )
    })
}

/// The test oracle for receiver_pass: the receiver's refusal of `entry` appended after the already-verified
/// keyring `history`, by folding the whole list; None only when the receiver accepts the whole list. A refusal at any other seq means the verified history and the
/// receiver disagree, which must never happen, so it refuses this entry too.
pub fn receiver_refusal(history: &[SyncEntry], entry: &SyncEntry) -> Option<ReceiverCheck> {
    let mut list: Vec<Value> = history.iter().map(SyncEntry::wire).collect();
    list.extend(wire(&[entry]));
    let is_fork = entry
        .body()
        .is_some_and(|body| body["kind"] == "fork-resolution");
    first_refusal_at(
        &if is_fork {
            receive_keyring_chain(&list)
        } else {
            fold_refusal(&list)
        },
        entry,
        &BTreeSet::from([entry.seq()]),
    )
}

/// The service receiver positioned after verified keyring history.
#[derive(Debug, Default)]
pub struct HistoryReceiver {
    receiver: super::vendor::sync_format::receive::KeyringReceiver,
    last_seq: u64,
    disagrees: bool,
}
impl HistoryReceiver {
    pub fn positioned_after(history: &[SyncEntry]) -> Self {
        let mut receiver = Self::default();
        for entry in history {
            if receiver.keyring(entry).is_some() {
                receiver.disagrees = true;
                break;
            }
        }
        receiver
    }
    pub fn keyring(&mut self, entry: &SyncEntry) -> Option<ReceiverCheck> {
        if self.disagrees {
            return Some(RECEIVER_SEQ_MISMATCH);
        }
        let refused = self.receiver.receive(&entry.wire());
        if refused.is_none() {
            self.last_seq = entry.seq();
        }
        refused
    }
    pub fn batch(&self, entry: &SyncEntry) -> Option<ReceiverCheck> {
        if self.disagrees {
            return Some(RECEIVER_SEQ_MISMATCH);
        }
        if entry.seq() <= self.last_seq {
            return Some(SEQ_NOT_INCREASING);
        }
        first_refusal_at(
            &fold_refusal(&[entry.wire()]),
            entry,
            &BTreeSet::from([entry.seq()]),
        )
    }
}
