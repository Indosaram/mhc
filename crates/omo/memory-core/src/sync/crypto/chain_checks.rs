use super::{
    entry::*, fold::*, links::next_link, parse::ParsedBody, signatures::author_signing_key,
};
#[derive(Clone, Copy)]
pub enum LinkMode {
    Main,
    KeyringOnly,
}
pub fn links_hold(head: Option<&Head>, entry: &SyncEntry, mode: LinkMode) -> bool {
    let seq = head.map_or(0, |h| h.seq);
    let f = entry.fields();
    match mode {
        LinkMode::Main => {
            if f.base_seq != seq
                || seq.checked_add(1) != Some(entry.seq())
                || next_link(head.map_or("", |h| h.link.as_str()), &f.digest).as_ref()
                    != Some(&f.link)
            {
                return false;
            }
        }
        LinkMode::KeyringOnly => {
            if entry.seq() <= seq || f.base_seq.checked_add(1) != Some(entry.seq()) {
                return false;
            }
        }
    }
    match entry {
        SyncEntry::Batch(_) => f.keyring_link.is_none(),
        SyncEntry::Keyring(_) => {
            f.keyring_link.is_some()
                && f.keyring_link
                    == next_link(head.map_or("", |h| h.keyring_link.as_str()), &f.digest)
        }
    }
}
pub fn signer_key<'a>(
    body: &'a ParsedBody,
    before: Option<&'a Authority>,
    author: &SyncAuthor,
    history: &'a [FoldedRecord],
) -> Option<&'a str> {
    if let ParsedBody::Genesis { first_device, .. } = body {
        return match author {
            SyncAuthor::Device { id, .. } if *id == first_device.id => {
                Some(&first_device.signing_public_key)
            }
            _ => None,
        };
    }
    let before = before?;
    if let ParsedBody::RecoveryJoin { credential_id, .. } = body {
        return match author {
            SyncAuthor::Recovery { credential_id: id } if id == credential_id => {
                author_signing_key(before, author)
            }
            _ => None,
        };
    }
    let SyncAuthor::Device { id, .. } = author else {
        return None;
    };
    if let ParsedBody::RecoveryJoinVetoed { join_digest } = body {
        return author_signing_key(
            history
                .iter()
                .find(|r| r.digest == *join_digest)?
                .before
                .as_ref()?,
            author,
        );
    }
    if is_cancelled_admission(before, id) {
        return None;
    }
    author_signing_key(before, author)
}
pub fn expected_epoch(body: &ParsedBody, before: Option<&Authority>) -> Option<u64> {
    match body {
        ParsedBody::Genesis { .. } => Some(1),
        ParsedBody::ForkResolution { epoch, .. } => Some(*epoch),
        ParsedBody::Rotation { .. } => before?.epoch.checked_add(1),
        _ => Some(before?.epoch),
    }
}
pub fn forced_rotation_refusal(
    after: Option<u64>,
    body: Option<&ParsedBody>,
) -> Option<&'static str> {
    let fork = match body {
        Some(ParsedBody::Rotation {
            fork_resolution, ..
        }) => *fork_resolution,
        _ => None,
    };
    match after {
        Some(seq) if fork != Some(seq) => Some("forced_rotation_required"),
        None if fork.is_some() => Some("bad_fork_resolution"),
        _ => None,
    }
}
pub fn key_within_epoch<'a>(
    history: &'a [FoldedRecord],
    current: Option<&'a Authority>,
    author: &SyncAuthor,
    epoch: u64,
) -> Option<&'a str> {
    for index in (0..history.len()).rev() {
        let a = history
            .get(index + 1)
            .and_then(|r| r.before.as_ref())
            .or(current);
        if let Some(a) = a.filter(|a| a.epoch == epoch)
            && let Some(key) = author_signing_key(a, author)
        {
            return Some(key);
        }
    }
    None
}
