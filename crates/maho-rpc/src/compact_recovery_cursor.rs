//! senpi `modes/rpc/compact-recovery-cursor.ts`: opaque bounded page cursors (RULING-62 Q2 A):
//! `<version>:<ordinal>:<offset>:<mac>`, an allow-listed ASCII id of at most 128 bytes whose
//! HMAC-SHA-256 authenticator binds the peer incarnation, routing session, token, generation,
//! component ref, ordinal, and raw byte offset.

use crate::compact_recovery_ids::parse_bounded_id;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// What a page cursor binds (RULING-62 Q2 A + (1)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorBinding {
    pub socket_epoch: String,
    pub session_id: String,
    pub token_id: String,
    pub generation: u64,
    pub component_ref: String,
}

/// Where a cursor continues: the ordinal of the page it was minted with and the next raw byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub ordinal: u64,
    pub offset: usize,
}

const CURSOR_MAC_BYTES: usize = 16;
const CURSOR_VERSION: &str = "p1";
const DOMAIN: &str = "senpi-recovery-cursor/1";

fn is_canonical_decimal(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    let bytes = s.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 16
        && matches!(bytes[0], b'1'..=b'9')
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

fn material(binding: &CursorBinding, position: CursorPosition) -> Vec<u8> {
    let gen_str = binding.generation.to_string();
    let ord_str = position.ordinal.to_string();
    let off_str = position.offset.to_string();
    let fields = [
        DOMAIN,
        &binding.socket_epoch,
        &binding.session_id,
        &binding.token_id,
        &gen_str,
        &binding.component_ref,
        &ord_str,
        &off_str,
    ];
    let mut out = Vec::new();
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            out.push(0);
        }
        out.extend_from_slice(format!("{}:{field}", field.len()).as_bytes());
    }
    out
}

pub struct RecoveryCursors {
    key: [u8; 32],
}

impl Default for RecoveryCursors {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryCursors {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        // Fall back to uuid entropy only if OS CSPRNG fails (practically unreachable on supported hosts).
        if getrandom::fill(&mut key).is_err() {
            let a = uuid::Uuid::new_v4();
            let b = uuid::Uuid::new_v4();
            key[..16].copy_from_slice(a.as_bytes());
            key[16..].copy_from_slice(b.as_bytes());
        }
        Self { key }
    }

    fn authenticate(&self, text: &[u8]) -> [u8; CURSOR_MAC_BYTES] {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .unwrap_or_else(|e| panic!("HMAC key length invariant: {e}"));
        mac.update(text);
        let full = mac.finalize().into_bytes();
        let mut truncated = [0u8; CURSOR_MAC_BYTES];
        truncated.copy_from_slice(&full[..CURSOR_MAC_BYTES]);
        truncated
    }

    pub fn mint(&self, binding: &CursorBinding, position: CursorPosition) -> String {
        let mac = self.authenticate(&material(binding, position));
        format!(
            "{CURSOR_VERSION}:{}:{}:{}",
            position.ordinal,
            position.offset,
            hex::encode(mac)
        )
    }

    pub fn open(&self, cursor: &str, binding: &CursorBinding) -> Option<CursorPosition> {
        if parse_bounded_id(cursor).is_err() {
            return None;
        }
        let mut parts = cursor.split(':');
        let version = parts.next()?;
        let ord_str = parts.next()?;
        let off_str = parts.next()?;
        let mac_hex = parts.next()?;
        if parts.next().is_some()
            || version != CURSOR_VERSION
            || !is_canonical_decimal(ord_str)
            || !is_canonical_decimal(off_str)
            || mac_hex.len() != CURSOR_MAC_BYTES * 2
            || !mac_hex
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let ordinal = ord_str.parse::<u64>().ok()?;
        let offset = off_str.parse::<usize>().ok()?;
        let position = CursorPosition { ordinal, offset };
        let expected = self.authenticate(&material(binding, position));
        let presented = hex::decode(mac_hex).ok()?;
        if presented.len() == expected.len() && bool::from(presented.ct_eq(&expected)) {
            Some(position)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> CursorBinding {
        CursorBinding {
            socket_epoch: "se-1".into(),
            session_id: "sess-1".into(),
            token_id: "tok-1".into(),
            generation: 2,
            component_ref: "c-0000000000000001:000".into(),
        }
    }

    #[test]
    fn mint_and_open_round_trips_and_enforces_binding() {
        let cursors = RecoveryCursors::new();
        let b = binding();
        let pos = CursorPosition {
            ordinal: 3,
            offset: 131_072,
        };
        let cursor = cursors.mint(&b, pos);
        assert!(parse_bounded_id(&cursor).is_ok());
        assert_eq!(cursors.open(&cursor, &b), Some(pos));

        let mut wrong = b.clone();
        wrong.generation = 3;
        assert_eq!(cursors.open(&cursor, &wrong), None);

        let other_cursors = RecoveryCursors::new();
        assert_eq!(other_cursors.open(&cursor, &b), None);
    }

    #[test]
    fn rejects_non_canonical_decimals_and_tampered_mac() {
        let cursors = RecoveryCursors::new();
        let b = binding();
        let cursor = cursors.mint(
            &b,
            CursorPosition {
                ordinal: 0,
                offset: 10,
            },
        );
        // Leading zero is non-canonical
        let bad_leading = cursor.replacen(":0:10:", ":00:10:", 1);
        assert_eq!(cursors.open(&bad_leading, &b), None);

        // Flip last hex char
        let mut tampered = cursor.clone();
        let last = tampered.pop().expect("hex");
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_eq!(cursors.open(&tampered, &b), None);
    }
}
