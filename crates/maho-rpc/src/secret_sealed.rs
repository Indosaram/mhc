//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-sealed.ts`.
//!
//! The engine half of sealed secret answers: omo-relay-spec decision 0022 section 3 (D11 secret
//! phase). Implemented from the decision text and TS port.

use crate::secret_sealed_hpke::X25519Dh;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `B` of 0022 3.2:
/// `["omo-secret-binding-v1", hin, hostJkt, durableSessionId, uiRequestId, deadlineAtMs, hpkePk, displayHash]`.
pub type SecretBinding = (
    &'static str,
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
);

#[derive(Debug, Clone)]
pub struct SecretBindingInput {
    pub host_instance: String,
    pub host_jkt: String,
    pub durable_session_id: String,
    pub ui_request_id: String,
    pub deadline_at_ms: i64,
    pub hpke_pk: String,
    pub title: String,
    pub placeholder: Option<String>,
}

/// Engine to holder (design 4.3): sent only to a sealed holder, whose grant negotiated `secret_input_v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPresenterSecretRequest {
    pub v: u8,
    pub session_id: String,
    pub durable_session_id: String,
    pub ui_request_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    pub deadline_at_ms: i64,
    pub host_instance: String,
    pub hpke_pk: String,
    pub host_pk: String,
    pub host_token: String,
    pub sig: String,
}

/// Engine to holder (design 3.2): a re-minted host token for every pending request of the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPresenterSecretToken {
    pub v: u8,
    pub host_instance: String,
    pub durable_session_id: String,
    pub host_token: String,
}

/// Outcome of a secret input request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretInputOutcome {
    Answered,
    Cancelled,
    TimedOut,
    Unavailable,
}

impl SecretInputOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "answered" => Some(Self::Answered),
            "cancelled" => Some(Self::Cancelled),
            "timed_out" => Some(Self::TimedOut),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Engine to holder (design 4.3, 7.2): value-free; `elsewhere` is false only for the winning holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPresenterSecretSettled {
    pub v: u8,
    pub ui_request_id: String,
    pub outcome: SecretInputOutcome,
    pub elsewhere: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpPresenterSecretFrame {
    #[serde(rename = "mcp_presenter_secret_request")]
    Request(McpPresenterSecretRequest),
    #[serde(rename = "mcp_presenter_secret_token")]
    Token(McpPresenterSecretToken),
    #[serde(rename = "mcp_presenter_secret_settled")]
    Settled(McpPresenterSecretSettled),
}

/// The per-request X25519 key of 0022 3.1: the public half as unpadded base64url of the raw 32 bytes.
#[derive(Clone)]
pub struct SecretRequestKey {
    pub hpke_pk: String,
    pub private_key: [u8; 32],
}

fn sha256_b64url(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()))
}

pub fn is_32_byte_b64url(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

/// `b64url(SHA-256(UTF-8(JSON.stringify([title, placeholder ?? null]))))`.
pub fn secret_display_hash(title: &str, placeholder: Option<&str>) -> String {
    let encoded = serde_json::to_string(&(title, placeholder)).unwrap_or_default();
    sha256_b64url(&encoded)
}

/// Builds `B`; refuses a deadline that is not a JS safe integer (`|d| <= 2^53 - 1`) and any key or
/// hash that is not 32 base64url bytes.
pub fn secret_binding(input: &SecretBindingInput) -> Result<SecretBinding, &'static str> {
    const MAX_SAFE_INT: i64 = (1i64 << 53) - 1;
    let display_hash = secret_display_hash(&input.title, input.placeholder.as_deref());
    if input.deadline_at_ms.abs() > MAX_SAFE_INT
        || !is_32_byte_b64url(&input.host_jkt)
        || !is_32_byte_b64url(&input.hpke_pk)
    {
        return Err("secret_binding_malformed");
    }
    Ok((
        "omo-secret-binding-v1",
        input.host_instance.clone(),
        input.host_jkt.clone(),
        input.durable_session_id.clone(),
        input.ui_request_id.clone(),
        input.deadline_at_ms,
        input.hpke_pk.clone(),
        display_hash,
    ))
}

fn serialize_binding(binding: &SecretBinding) -> String {
    serde_json::to_string(binding).unwrap_or_default()
}

/// `req = b64url(SHA-256(UTF-8(JSON.stringify(B))))`: names one request everywhere an answer is bound to it.
pub fn secret_request_hash(binding: &SecretBinding) -> String {
    sha256_b64url(&serialize_binding(binding))
}

/// HPKE `info`: `UTF-8("omo-secret-answer-v1") || 0x00 || req` (its 32 bytes).
pub fn secret_answer_info(req: &str) -> Vec<u8> {
    let req_bytes = URL_SAFE_NO_PAD.decode(req).unwrap_or_default();
    let mut out = Vec::with_capacity("omo-secret-answer-v1".len() + 1 + req_bytes.len());
    out.extend_from_slice(b"omo-secret-answer-v1");
    out.push(0);
    out.extend_from_slice(&req_bytes);
    out
}

/// HPKE AAD: `UTF-8(JSON.stringify(["omo-secret-answer-aad-v1", req, principalId, grantId, answerId, stepupJti]))`.
pub fn secret_answer_aad(
    req: &str,
    principal_id: &str,
    grant_id: &str,
    answer_id: &str,
    jti: &str,
) -> Vec<u8> {
    serde_json::to_vec(&(
        "omo-secret-answer-aad-v1",
        req,
        principal_id,
        grant_id,
        answer_id,
        jti,
    ))
    .unwrap_or_default()
}

/// What the installation key signs for a value answer; the ciphertext enters as `b64url(SHA-256(enc || ct))`.
pub fn secret_answer_transcript(
    req: &str,
    principal_id: &str,
    grant_id: &str,
    answer_id: &str,
    jti: &str,
    enc: &[u8],
    ct: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(enc);
    hasher.update(ct);
    let sealed = URL_SAFE_NO_PAD.encode(hasher.finalize());
    let payload = serde_json::to_string(&(req, principal_id, grant_id, answer_id, jti, sealed))
        .unwrap_or_default();
    format!("omo-secret-answer-v1\n{payload}")
}

/// What the installation key signs for a cancel.
pub fn secret_cancel_transcript(
    req: &str,
    principal_id: &str,
    grant_id: &str,
    answer_id: &str,
) -> String {
    let payload = serde_json::to_string(&(req, principal_id, grant_id, answer_id, "cancel"))
        .unwrap_or_default();
    format!("omo-secret-answer-v1\n{payload}")
}

/// The canonical plaintext only: `u32be(byteLength) || UTF-8(value) || zeros`, exactly the next
/// power of two of at least 256 and at most 64 KiB, all-zero padding and valid UTF-8 (a leading
/// BOM is kept). Anything else is `None`.
pub fn unpad_secret_value(plaintext: &[u8]) -> Option<String> {
    if plaintext.len() < 256 || plaintext.len() > 65536 {
        return None;
    }
    let length = u32::from_be_bytes(plaintext[..4].try_into().ok()?) as usize;
    let total = 4usize.checked_add(length)?;
    let mut size = 256usize;
    while size < total {
        size = size.checked_mul(2)?;
    }
    if size != plaintext.len() || plaintext[total..].iter().any(|&b| b != 0) {
        return None;
    }
    std::str::from_utf8(&plaintext[4..total])
        .ok()
        .map(str::to_owned)
}

/// The host key signs exactly this: `"omo-secret-request-v1\n" + JSON.stringify(B)`.
pub fn secret_request_transcript(binding: &SecretBinding) -> String {
    format!("omo-secret-request-v1\n{}", serialize_binding(binding))
}

/// Generates a fresh per-request X25519 key through the injected [`X25519Dh`] provider.
pub async fn generate_secret_request_key(dh: &dyn X25519Dh) -> Result<SecretRequestKey, String> {
    let (pub_raw, private_key) = dh.generate_keypair()?;
    if pub_raw.len() != 32 {
        return Err("secret_request_key_unavailable".to_owned());
    }
    Ok(SecretRequestKey {
        hpke_pk: URL_SAFE_NO_PAD.encode(&pub_raw),
        private_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_and_transcripts_match_spec_format() {
        let key = URL_SAFE_NO_PAD.encode([7u8; 32]);
        let binding = secret_binding(&SecretBindingInput {
            host_instance: "hin".into(),
            host_jkt: key.clone(),
            durable_session_id: "sid".into(),
            ui_request_id: "uid".into(),
            deadline_at_ms: 1_700_000_000_000,
            hpke_pk: key.clone(),
            title: "API key".into(),
            placeholder: None,
        })
        .expect("binding");
        let tx = secret_request_transcript(&binding);
        assert!(tx.starts_with("omo-secret-request-v1\n[\"omo-secret-binding-v1\","));
        let req = secret_request_hash(&binding);
        assert!(is_32_byte_b64url(&req));
        let info = secret_answer_info(&req);
        assert_eq!(&info[..21], b"omo-secret-answer-v1\0");
        assert_eq!(info.len(), 21 + 32);
    }

    #[test]
    fn unpad_enforces_power_of_two_zero_padding_and_utf8() {
        let mut buf = vec![0u8; 256];
        let secret = "\u{feff}sk-test";
        buf[..4].copy_from_slice(&(secret.len() as u32).to_be_bytes());
        buf[4..4 + secret.len()].copy_from_slice(secret.as_bytes());
        assert_eq!(unpad_secret_value(&buf).as_deref(), Some(secret));

        // Non-zero padding byte is rejected.
        let mut dirty = buf.clone();
        dirty[255] = 1;
        assert_eq!(unpad_secret_value(&dirty), None);

        // Wrong size (512 for short secret) is rejected.
        let mut wrong_size = vec![0u8; 512];
        wrong_size[..4].copy_from_slice(&1u32.to_be_bytes());
        wrong_size[4] = b'x';
        assert_eq!(unpad_secret_value(&wrong_size), None);

        // Invalid UTF-8 is rejected.
        let mut bad_utf8 = vec![0u8; 256];
        bad_utf8[..4].copy_from_slice(&2u32.to_be_bytes());
        bad_utf8[4] = 0xff;
        bad_utf8[5] = 0xfe;
        assert_eq!(unpad_secret_value(&bad_utf8), None);
    }
}
