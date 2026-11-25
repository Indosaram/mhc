//! The authority word of a host-mode delegated connection (omo-relay-daemon#307).
//!
//! One two-word atomic buffer per incarnation of a delegated socket: index 0 packs
//! `epoch << 4 | retired << 3 | mask` (mask bit 0 = `interactive_user`, bit 1 = `task_parent`,
//! bit 2 = `peer_agent`), index 1 is the incarnation, stamped once before the buffer is shared.
//! The host thread is the only writer, and every write is monotone within a word: mask bits are
//! only cleared, retired is only set, the epoch only grows and changes only on a real transition.
//! Host and worker threads read the same memory with one atomic load per command, so the load at
//! a command's first role check is its admission and the host's store is the revocation point.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// The roles a word can carry, in bit order.
const WORD_ROLES: [&str; 3] = ["interactive_user", "task_parent", "peer_agent"];
const MASK_BITS: u32 = 7;
const RETIRED_BIT: u32 = 8;
const EPOCH_SHIFT: u32 = 4;
/// The epoch field holds 28 bits; within one incarnation it never passes 4, so this bound is an invariant check.
const EPOCH_LIMIT: u32 = 1 << 28;

/// The largest incarnation slot [1] holds unchanged.
pub const INCARNATION_MAX: i32 = 0x7fff_ffff;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    #[error("authority_epoch_exhausted")]
    EpochExhausted,
    #[error("authority_incarnation_invalid")]
    IncarnationInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityState {
    pub epoch: u32,
    pub retired: bool,
    pub mask: u32,
    pub roles: Vec<String>,
}

/// Two-word atomic authority buffer matching TS `Int32Array(SharedArrayBuffer(8))`.
#[derive(Debug)]
pub struct AuthorityBuffer {
    words: [AtomicI32; 2],
}

pub fn mask_of_roles<'a>(roles: impl IntoIterator<Item = &'a str>) -> u32 {
    let mut mask = 0u32;
    for role in roles {
        if let Some(bit) = WORD_ROLES.iter().position(|known| *known == role) {
            mask |= 1u32 << bit;
        }
    }
    mask
}

pub fn roles_of_mask(mask: u32) -> Vec<String> {
    WORD_ROLES
        .iter()
        .enumerate()
        .filter(|(bit, _)| (mask & (1u32 << bit)) != 0)
        .map(|(_, role)| (*role).to_owned())
        .collect()
}

pub fn decode_word(word: i32) -> AuthorityState {
    let bits = word as u32;
    let mask = bits & MASK_BITS;
    AuthorityState {
        epoch: bits >> EPOCH_SHIFT,
        retired: (bits & RETIRED_BIT) != 0,
        mask,
        roles: roles_of_mask(mask),
    }
}

fn pack_word(epoch: u32, retired: bool, mask: u32) -> Result<i32, AuthorityError> {
    if !(1..EPOCH_LIMIT).contains(&epoch) {
        return Err(AuthorityError::EpochExhausted);
    }
    let word =
        (epoch << EPOCH_SHIFT) | (if retired { RETIRED_BIT } else { 0 }) | (mask & MASK_BITS);
    Ok(word as i32)
}

/// A new incarnation's word: epoch 1, not retired, `mask`, with `incarnation` stamped before anything shares it.
pub fn create_authority(
    incarnation: i32,
    mask: u32,
) -> Result<Arc<AuthorityBuffer>, AuthorityError> {
    if !(1..=INCARNATION_MAX).contains(&incarnation) {
        return Err(AuthorityError::IncarnationInvalid);
    }
    let word = pack_word(1, false, mask & MASK_BITS)?;
    Ok(Arc::new(AuthorityBuffer {
        words: [AtomicI32::new(word), AtomicI32::new(incarnation)],
    }))
}

pub fn read_authority(buffer: &AuthorityBuffer) -> AuthorityState {
    decode_word(buffer.words[0].load(Ordering::SeqCst))
}

pub fn incarnation_of(buffer: &AuthorityBuffer) -> i32 {
    buffer.words[1].load(Ordering::SeqCst)
}

/// Host thread only: `mask <- mask & roles`. The epoch moves only when a bit cleared; `true` when it did.
pub fn store_narrowed<'a>(
    buffer: &AuthorityBuffer,
    roles: impl IntoIterator<Item = &'a str>,
) -> Result<bool, AuthorityError> {
    let current = decode_word(buffer.words[0].load(Ordering::SeqCst));
    let mask = current.mask & mask_of_roles(roles);
    if mask == current.mask {
        return Ok(false);
    }
    let word = pack_word(current.epoch + 1, current.retired, mask)?;
    buffer.words[0].store(word, Ordering::SeqCst);
    Ok(true)
}

/// Host thread only: sets retired and moves the epoch; a word already retired is left as it is (`false`).
pub fn store_retired(buffer: &AuthorityBuffer) -> Result<bool, AuthorityError> {
    let current = decode_word(buffer.words[0].load(Ordering::SeqCst));
    if current.retired {
        return Ok(false);
    }
    let word = pack_word(current.epoch + 1, true, current.mask)?;
    buffer.words[0].store(word, Ordering::SeqCst);
    Ok(true)
}

/// The successor of incarnation `k`, checked BEFORE any stamp; `None` once the counter is exhausted (never wraps).
pub fn next_incarnation(k: i32, max: Option<i32>) -> Option<i32> {
    let limit = max.unwrap_or(INCARNATION_MAX);
    (k >= 1 && k < limit).then(|| k + 1)
}

/// Whether a word admits roles for a claim: claimed, not retired, and still in the mask.
pub fn effective_roles(claim: Option<&[String]>, state: &AuthorityState) -> Vec<String> {
    if state.retired {
        return Vec::new();
    }
    let Some(claim) = claim else {
        return Vec::new();
    };
    claim
        .iter()
        .filter(|role| state.roles.iter().any(|allowed| allowed == *role))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_words_round_trip() {
        let mask = mask_of_roles(["peer_agent", "interactive_user", "unknown"]);
        assert_eq!(mask, 0b101);
        assert_eq!(roles_of_mask(mask), vec!["interactive_user", "peer_agent"]);

        let buf = create_authority(42, mask).expect("create");
        assert_eq!(incarnation_of(&buf), 42);
        let state = read_authority(&buf);
        assert_eq!(state.epoch, 1);
        assert!(!state.retired);
        assert_eq!(state.mask, 0b101);
        assert_eq!(state.roles, vec!["interactive_user", "peer_agent"]);
    }

    #[test]
    fn narrowing_and_retiring_are_monotone_and_bump_epoch() {
        let buf = create_authority(1, mask_of_roles(["interactive_user", "task_parent"]))
            .expect("create");
        assert!(
            !store_narrowed(&buf, ["interactive_user", "task_parent", "peer_agent"]).expect("same")
        );
        assert_eq!(read_authority(&buf).epoch, 1);

        assert!(store_narrowed(&buf, ["task_parent"]).expect("narrow"));
        let after = read_authority(&buf);
        assert_eq!(after.epoch, 2);
        assert_eq!(after.roles, vec!["task_parent"]);
        assert!(!after.retired);

        assert!(store_retired(&buf).expect("retire"));
        let retired = read_authority(&buf);
        assert_eq!(retired.epoch, 3);
        assert!(retired.retired);
        assert!(!store_retired(&buf).expect("idempotent"));
        assert_eq!(read_authority(&buf).epoch, 3);
    }

    #[test]
    fn effective_roles_respects_retired_and_incarnation_bounds() {
        let claim = vec!["interactive_user".to_owned(), "peer_agent".to_owned()];
        let active = AuthorityState {
            epoch: 1,
            retired: false,
            mask: 0b001,
            roles: vec!["interactive_user".to_owned()],
        };
        assert_eq!(
            effective_roles(Some(&claim), &active),
            vec!["interactive_user"]
        );
        assert!(effective_roles(None, &active).is_empty());

        let retired = AuthorityState {
            retired: true,
            ..active
        };
        assert!(effective_roles(Some(&claim), &retired).is_empty());

        assert_eq!(next_incarnation(1, None), Some(2));
        assert_eq!(next_incarnation(INCARNATION_MAX, None), None);
        assert_eq!(next_incarnation(0, None), None);
        assert!(matches!(
            create_authority(0, 1),
            Err(AuthorityError::IncarnationInvalid)
        ));
    }
}
