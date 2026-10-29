use super::{
    fold::{KeyringFold, ReceivedChain, ReceiverRefusal},
    fork_evidence::verify_fork_evidence,
};
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct KeyringReceiver {
    fold: KeyringFold,
}
pub fn create_keyring_receiver() -> KeyringReceiver {
    KeyringReceiver::default()
}
impl KeyringReceiver {
    pub fn fold(&self) -> super::fold::AuthorityFold {
        self.fold.authority.clone()
    }
    /// A refusal leaves the receiver unchanged, including its sequence position.
    pub fn receive(&mut self, entry: &Value) -> Option<ReceiverRefusal> {
        if let Some(reason) = self.fold.refusal(entry) {
            return Some(reason);
        }
        if entry["kind"] == "keyring" && entry["body"]["kind"] == "fork-resolution"
            && let Some(evidence) = entry["body"].get("evidence") {
                let ancestry = self
                    .fold
                    .through(evidence["common"]["seq"].as_u64().unwrap_or(0));
                if !verify_fork_evidence(ancestry, evidence) {
                    return Some("fork_evidence_invalid");
                }
            }
        self.fold.accept(entry);
        None
    }
}
pub fn receive_keyring_chain(entries: &[Value]) -> ReceivedChain {
    let mut receiver = create_keyring_receiver();
    for entry in entries {
        if let Some(reason) = receiver.receive(entry) {
            return Err((entry["seq"].as_u64().unwrap_or(0), reason));
        }
    }
    Ok(receiver.fold.authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn incremental_receiver_matches_whole_chain_and_refusal_does_not_advance() {
        let genesis = json!({"seq":1,"kind":"keyring","author":{"kind":"device","id":"a"},"digest":"g","body":{"kind":"genesis","bootstrap":{"first_device":{"id":"a"},"recovery_key":{"credential_id":"r"}}}});
        let approval = json!({"seq":2,"kind":"keyring","author":{"kind":"device","id":"a"},"digest":"a","body":{"kind":"approval","principal":{"id":"b"}}});
        let mut receiver = create_keyring_receiver();
        assert_eq!(receiver.receive(&genesis), None);
        let bad = json!({"seq":2,"kind":"batch","author":{"kind":"device"},"body":{}});
        assert_eq!(receiver.receive(&bad), Some("batch_with_body"));
        assert_eq!(receiver.receive(&approval), None);
        assert_eq!(receiver.receive(&approval), Some("seq_not_increasing"));
        let full = receive_keyring_chain(&[genesis, approval]).expect("whole chain");
        assert_eq!(receiver.fold().members, full.members);
        assert_eq!(receiver.fold().registry, full.registry);
    }
}
