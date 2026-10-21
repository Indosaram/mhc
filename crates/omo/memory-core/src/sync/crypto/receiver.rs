use super::{
    entry::SyncEntry,
    vendor::sync_format::{fold::fold_refusal, receive::receive_keyring_chain},
};
pub fn receiver_refusal(history: &[SyncEntry], entry: &SyncEntry) -> Option<&'static str> {
    let mut list: Vec<_> = history.iter().map(SyncEntry::wire).collect();
    list.push(entry.wire());
    let fork = entry.body().is_some_and(|b| b["kind"] == "fork-resolution");
    match if fork {
        receive_keyring_chain(&list)
    } else {
        fold_refusal(&list)
    } {
        Ok(_) => None,
        Err((seq, reason)) => Some(if seq == entry.seq() {
            reason
        } else {
            "receiver_seq_mismatch"
        }),
    }
}
