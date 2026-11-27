//! Port of senpi packages/coding-agent/src/core/session-affinity-key.ts.

use std::path::Path;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::install_salt::install_salt;

const SALT_FILE: &str = "session-affinity-salt";

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut k = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        k[..32].copy_from_slice(&hashed);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let final_digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&final_digest);
    out
}

pub fn session_affinity_key(session_id: &str, agent_dir: &Path) -> String {
    let salt = install_salt(agent_dir, SALT_FILE);
    let mut msg = Vec::with_capacity("session-affinity\0".len() + session_id.len());
    msg.extend_from_slice(b"session-affinity\0");
    msg.extend_from_slice(session_id.as_bytes());
    hex::encode(hmac_sha256(&salt, &msg))
}

fn claude_code_user_id(key: &str) -> String {
    let session = format!(
        "{}-{}-{}-{}-{}",
        &key[..8],
        &key[8..12],
        &key[12..16],
        &key[16..20],
        &key[20..32]
    );
    format!("user_{key}_account__session_{session}")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionAffinityStreamOptions {
    pub session_affinity_key: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

/// Stream options that pin an Anthropic Messages session to one upstream account behind a multi-account gateway.
pub fn session_affinity_stream_options(
    api: &str,
    compat_send_session_affinity_headers: Option<bool>,
    session_id: Option<&str>,
    existing_metadata: Option<&Map<String, Value>>,
    enabled: bool,
    agent_dir: &Path,
) -> SessionAffinityStreamOptions {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return SessionAffinityStreamOptions::default();
    };
    if !enabled
        || api != "anthropic-messages"
        || compat_send_session_affinity_headers == Some(false)
    {
        return SessionAffinityStreamOptions::default();
    }
    let key = session_affinity_key(session_id, agent_dir);
    if existing_metadata
        .and_then(|meta| meta.get("user_id"))
        .is_some_and(Value::is_string)
    {
        return SessionAffinityStreamOptions {
            session_affinity_key: Some(key),
            metadata: None,
        };
    }
    let mut metadata = existing_metadata.cloned().unwrap_or_default();
    metadata.insert(
        "user_id".to_owned(),
        Value::String(claude_code_user_id(&key)),
    );
    SessionAffinityStreamOptions {
        session_affinity_key: Some(key),
        metadata: Some(metadata),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_stable_affinity_key_and_metadata_for_anthropic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opts = session_affinity_stream_options(
            "anthropic-messages",
            None,
            Some("sess-1"),
            None,
            true,
            dir.path(),
        );
        let key = opts.session_affinity_key.clone().expect("key");
        assert_eq!(key.len(), 64);
        let user_id = opts
            .metadata
            .as_ref()
            .and_then(|m| m.get("user_id"))
            .and_then(Value::as_str)
            .expect("user_id");
        assert!(user_id.starts_with(&format!("user_{key}_account__session_")));

        // Disabled or non-anthropic returns empty
        let off = session_affinity_stream_options(
            "openai-responses",
            None,
            Some("sess-1"),
            None,
            true,
            dir.path(),
        );
        assert_eq!(off, SessionAffinityStreamOptions::default());
    }
}
