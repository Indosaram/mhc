use super::{
    anchor::*,
    chain_checks::*,
    entry::*,
    envelope::{envelope_refusal, parse_entry},
    fold::*,
    parse::{ParsedBody, parse_body},
    receiver::HistoryReceiver,
    rejection::RejectionCounter,
    rules::check_record_rules,
    signatures::*,
};
use serde_json::Value;
pub type ChainRejection = &'static str;
pub const UNTRUSTED_STATE: ChainRejection = "untrusted_state";
#[derive(Debug, Clone)]
pub struct ChainState {
    account: String,
    receiver: std::sync::Arc<std::sync::Mutex<Option<HistoryReceiver>>>,
    head: Option<Head>,
    authority: Option<Authority>,
    history: Vec<FoldedRecord>,
    received: Vec<VerifiedKeyringEntry>,
    anchor: ChainAnchor,
    anchored: bool,
    unanchored: Vec<KeyringEntry>,
    genesis_digest: Option<String>,
    pin: Option<PinCandidate>,
    forced_rotation_after: Option<u64>,
}
impl ChainState {
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn head(&self) -> Option<&Head> {
        self.head.as_ref()
    }
    pub fn authority(&self) -> Option<&Authority> {
        self.authority.as_ref()
    }
    pub fn received(&self) -> &[VerifiedKeyringEntry] {
        &self.received
    }
    pub fn anchored(&self) -> bool {
        self.anchored
    }
    pub fn pin(&self) -> Option<&PinCandidate> {
        self.pin.as_ref()
    }
}
#[derive(Debug, Clone)]
pub struct AcceptedBatch {
    pub seq: u64,
    pub digest: String,
    pub epoch: u64,
    pub author_id: String,
    pub signing_public_key: String,
}
#[derive(Debug, Clone)]
pub enum ChainStep {
    Keyring { seq: u64, record: &'static str },
    Batch(AcceptedBatch),
    Vetoed { seq: u64 },
}
#[derive(Debug)]
pub enum AppendOutcome {
    Accepted {
        state: ChainState,
        steps: Vec<ChainStep>,
    },
    Rejected {
        state: ChainState,
        steps: Vec<ChainStep>,
        seq: u64,
        reason: ChainRejection,
    },
}
pub fn empty_chain(account: String, anchor: ChainAnchor) -> ChainState {
    ChainState {
        account,
        receiver: Default::default(),
        head: None,
        authority: None,
        history: Vec::new(),
        received: Vec::new(),
        anchor,
        anchored: false,
        unanchored: Vec::new(),
        genesis_digest: None,
        pin: None,
        forced_rotation_after: None,
    }
}
pub(super) fn at_verified_head(state: &ChainState, head: Head) -> Option<ChainState> {
    let old = state.head.as_ref()?;
    if !state.anchored || old.keyring_link != head.keyring_link || head.seq < old.seq {
        return None;
    }
    if head.seq == old.seq && head.link != old.link {
        return None;
    }
    let mut next = state.clone();
    let receiver = state.receiver.lock().expect("receiver lock").take();
    next.receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
    next.head = Some(head);
    Some(next)
}
fn mint_wraps(entry: KeyringEntry) -> VerifiedKeyringEntry {
    let wraps = entry
        .wraps
        .iter()
        .map(|w| VerifiedWrap {
            entry: w.clone(),
            account: entry.fields.account.clone(),
        })
        .collect();
    VerifiedKeyringEntry { entry, wraps }
}
fn seen_keyring(state: &ChainState) -> Vec<SyncEntry> {
    state
        .received
        .iter()
        .map(|e| SyncEntry::Keyring(e.entry.clone()))
        .chain(state.unanchored.iter().cloned().map(SyncEntry::Keyring))
        .collect()
}
fn take_receiver(state: &ChainState) -> HistoryReceiver {
    state
        .receiver
        .lock()
        .expect("receiver lock")
        .take()
        .unwrap_or_else(|| HistoryReceiver::positioned_after(&seen_keyring(state)))
}
pub(super) fn verify_keyring_entry(
    state: &ChainState,
    entry: &KeyringEntry,
    mode: LinkMode,
) -> Result<(ChainState, ChainStep), ChainRejection> {
    let wire = SyncEntry::Keyring(entry.clone());
    let mut receiver = take_receiver(state);
    let result = verify_keyring_wire(state, &wire, mode, &mut receiver);
    if let Ok((next, _)) = &result {
        *next.receiver.lock().expect("receiver lock") = Some(receiver);
    }
    result
}
/// Consults the carried service receiver before the client's cryptographic checks.
fn verify_keyring_wire(
    state: &ChainState,
    wire: &SyncEntry,
    mode: LinkMode,
    receiver: &mut HistoryReceiver,
) -> Result<(ChainState, ChainStep), ChainRejection> {
    let SyncEntry::Keyring(entry) = wire else {
        return Err("bad_envelope");
    };
    if let Some(r) = envelope_refusal(wire) {
        return Err(r);
    }
    if state.anchor.account() != state.account {
        return Err("untrusted_root");
    }
    if !links_hold(state.head.as_ref(), wire, mode) {
        return Err("link_mismatch");
    }
    if let Some(r) = receiver.keyring(wire) {
        return Err(r);
    }
    let body = parse_body(&entry.body).ok_or("bad_record")?;
    let before = state.authority.as_ref();
    let key = signer_key(&body, before, &entry.author, &state.history).ok_or("not_member")?;
    if !verify_record_sig(key, wire) {
        return Err("bad_signature");
    }
    if Some(entry.fields.epoch) != expected_epoch(&body, before)
        || super::envelope::integer(&entry.body["epoch"], 1) != Some(entry.fields.epoch)
    {
        return Err("epoch_mismatch");
    }
    if entry.fields.account != state.account
        || entry.body["account"].as_str() != Some(&state.account)
    {
        return Err("account_mismatch");
    }
    let predecessor = state.head.as_ref().map_or("", |h| h.keyring_link.as_str());
    if let Some(r) = check_record_rules(entry, &body, before, &state.history, predecessor) {
        return Err(r);
    }
    if sha256_b64(&canonical(&entry.body).map_err(|_| "bad_record")?) != entry.fields.digest {
        return Err("digest_mismatch");
    }
    if let Some(r) = forced_rotation_refusal(state.forced_rotation_after, Some(&body)) {
        return Err(r);
    }
    let genesis = if matches!(body, ParsedBody::Genesis { .. }) {
        Some(entry.fields.digest.clone())
    } else {
        state.genesis_digest.clone()
    };
    let met = if state.anchored {
        (false, None)
    } else {
        anchor_step(
            &state.anchor,
            entry,
            &body,
            before,
            genesis.as_deref().unwrap_or(""),
        )?
    };
    let record = FoldedRecord {
        seq: entry.seq,
        digest: entry.fields.digest.clone(),
        body: body.clone(),
        before: before.cloned(),
    };
    let authority = apply_record(&state.history, &record).ok_or("bad_record")?;
    let mut next = state.clone();
    next.receiver = Default::default();
    next.head = Some(Head {
        seq: entry.seq,
        link: entry.fields.link.clone(),
        keyring_link: entry.fields.keyring_link.clone().ok_or("bad_envelope")?,
    });
    next.authority = Some(authority);
    next.history.push(record);
    next.anchored |= met.0;
    if next.anchored {
        next.received
            .extend(next.unanchored.drain(..).map(mint_wraps));
        next.received.push(mint_wraps(entry.clone()));
    } else {
        next.unanchored.push(entry.clone());
    }
    next.genesis_digest = genesis;
    if met.1.is_some() {
        next.pin = met.1;
    }
    next.forced_rotation_after = if matches!(body, ParsedBody::ForkResolution { .. }) {
        Some(entry.seq)
    } else {
        None
    };
    Ok((
        next,
        ChainStep::Keyring {
            seq: entry.seq,
            record: body.kind(),
        },
    ))
}
fn verify_batch_entry(
    state: &ChainState,
    wire: &SyncEntry,
    receiver: &HistoryReceiver,
) -> Result<(ChainState, ChainStep), ChainRejection> {
    let SyncEntry::Batch(entry) = wire else {
        return Err("bad_envelope");
    };
    if let Some(r) = envelope_refusal(wire) {
        return Err(r);
    }
    if !links_hold(state.head.as_ref(), wire, LinkMode::Main) {
        return Err("link_mismatch");
    }
    if let Some(r) = receiver.batch(wire) {
        return Err(r);
    }
    if let Some(r) = forced_rotation_refusal(state.forced_rotation_after, None) {
        return Err(r);
    }
    let a = state.authority.as_ref().ok_or("not_member")?;
    let head = state.head.as_ref().ok_or("not_member")?;
    let SyncAuthor::Device { id, .. } = &entry.author else {
        return Err("not_member");
    };
    let vetoed = is_vetoed_epoch(a, entry.fields.epoch) || is_cancelled_admission(a, id);
    let key = if vetoed {
        key_within_epoch(&state.history, Some(a), &entry.author, entry.fields.epoch)
    } else {
        author_signing_key(a, &entry.author)
    }
    .ok_or("not_member")?;
    if !verify_record_sig(key, wire) {
        return Err("bad_signature");
    }
    if entry.fields.account != state.account {
        return Err("account_mismatch");
    }
    let mut next = state.clone();
    next.receiver = Default::default();
    next.head = Some(Head {
        seq: entry.seq,
        link: entry.fields.link.clone(),
        keyring_link: head.keyring_link.clone(),
    });
    if vetoed {
        return Ok((next, ChainStep::Vetoed { seq: entry.seq }));
    }
    if entry.fields.epoch != a.epoch {
        return Err("epoch_mismatch");
    }
    Ok((
        next,
        ChainStep::Batch(AcceptedBatch {
            seq: entry.seq,
            digest: entry.fields.digest.clone(),
            epoch: entry.fields.epoch,
            author_id: id.clone(),
            signing_public_key: key.into(),
        }),
    ))
}
pub fn append_entries(
    state: &ChainState,
    entries: &[Value],
    rejections: &mut RejectionCounter,
) -> AppendOutcome {
    let mut current = state.clone();
    let mut steps = Vec::new();
    let parsed_rows: Vec<Result<SyncEntry, &'static str>> =
        entries.iter().map(parse_entry).collect();
    let mut receiver = take_receiver(state);
    for parsed in &parsed_rows {
        let seq = parsed.as_ref().map_or_else(
            |_| current.head.as_ref().map_or(1, |h| h.seq.saturating_add(1)),
            SyncEntry::seq,
        );
        let result = match parsed {
            Err(reason) => Err(*reason),
            Ok(entry) => match entry {
                SyncEntry::Keyring(_) => {
                    verify_keyring_wire(&current, entry, LinkMode::Main, &mut receiver)
                }
                SyncEntry::Batch(_) => verify_batch_entry(&current, entry, &receiver),
            },
        };
        match result {
            Ok((next, step)) => {
                current = next;
                steps.push(step);
            }
            Err(reason) => {
                rejections.record(reason);
                return if current.anchored {
                    AppendOutcome::Rejected {
                        state: current,
                        steps,
                        seq,
                        reason,
                    }
                } else {
                    AppendOutcome::Rejected {
                        state: state.clone(),
                        steps: Vec::new(),
                        seq,
                        reason,
                    }
                };
            }
        }
    }
    if !current.anchored {
        rejections.record("unanchored");
        return AppendOutcome::Rejected {
            state: state.clone(),
            steps: Vec::new(),
            seq: current.head.as_ref().map_or(1, |h| h.seq.saturating_add(1)),
            reason: "unanchored",
        };
    }
    *current.receiver.lock().expect("receiver lock") = Some(receiver);
    AppendOutcome::Accepted {
        state: current,
        steps,
    }
}
