use crate::sync::crypto::{chain::ChainState, entry::KeyringEntry, parse::parse_principal};
pub const VETO_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
#[derive(Debug, PartialEq, Eq)]
pub enum JoinNotice {
    NotAJoin,
    Behind,
    Ordinary,
    Foreign {
        join_digest: String,
        prompt: &'static str,
        actions: [&'static str; 2],
    },
    Excluded {
        join_digest: String,
        prompt: &'static str,
        actions: [&'static str; 2],
    },
}
pub fn classify_recovery_join(
    state: &ChainState,
    join: &KeyringEntry,
    self_id: &str,
) -> JoinNotice {
    if join.body["kind"] != "recovery-join" {
        return JoinNotice::NotAJoin;
    }
    let Some(head) = state.head() else {
        return JoinNotice::Behind;
    };
    if head.seq < join.fields.base_seq {
        return JoinNotice::Behind;
    }
    let prompt = "a-device-joined-using-your-passkey";
    let actions = ["veto", "confirm"];
    let link_at_base = state
        .received()
        .iter()
        .take_while(|entry| entry.entry.seq <= join.fields.base_seq)
        .last()
        .and_then(|entry| entry.entry.fields.keyring_link.as_deref());
    if (head.seq >= join.seq
        && state
            .received()
            .iter()
            .find(|entry| entry.entry.seq == join.seq)
            .map(|entry| &entry.entry.fields.digest)
            != Some(&join.fields.digest))
        || link_at_base != join.body["keyring_head"].as_str()
    {
        return JoinNotice::Foreign {
            join_digest: join.fields.digest.clone(),
            prompt,
            actions,
        };
    }
    if parse_principal(&join.body["principal"]).is_some_and(|p| p.id == self_id) {
        JoinNotice::Ordinary
    } else {
        JoinNotice::Excluded {
            join_digest: join.fields.digest.clone(),
            prompt,
            actions,
        }
    }
}
