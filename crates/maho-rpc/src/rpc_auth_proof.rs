//! The endpoint proof both RPC hops share (senpi `rpc-auth-proof.ts`, wire contract v1.1): one HMAC-SHA256 over the
//! newline-joined label and parts, compared in constant time.
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const RPC_AUTH_CAPABILITY: &str = "socket_auth";

/// senpi `RPC_AUTH_HEX`: exactly 64 lowercase hex digits.
pub fn is_rpc_auth_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// senpi `rpcAuthProof(key, label, ...parts)`: hex HMAC-SHA256 of `senpi-rpc-<label>\n<part>\n...`.
pub fn rpc_auth_proof(key: &[u8], label: &str, parts: &[&str]) -> String {
    let mut message = format!("senpi-rpc-{label}");
    for part in parts {
        message.push('\n');
        message.push_str(part);
    }
    // HMAC accepts a key of any length.
    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(key) {
        Ok(mac) => mac,
        Err(_) => unreachable!("HMAC takes keys of any length"),
    };
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// senpi `rpcProofMatches`: the received value must be a 64-digit lowercase hex string, compared in constant time.
pub fn rpc_proof_matches(expected: &str, received: Option<&serde_json::Value>) -> bool {
    let Some(received) = received.and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !is_rpc_auth_hex(received) {
        return false;
    }
    let (Ok(expected), Ok(received)) = (hex::decode(expected), hex::decode(received)) else {
        return false;
    };
    expected.len() == received.len() && bool::from(expected.as_slice().ct_eq(received.as_slice()))
}

/// senpi `authRecord`: a JSON object, never an array or null.
pub fn auth_record(
    value: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value.as_object()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proof_matches_an_independent_hmac_vector() {
        // python: hmac.new(b"\x07"*32, b"senpi-rpc-server\ninternal\n" + b"a"*64 + b"\n" + b"b"*64, sha256)
        let proof = rpc_auth_proof(
            &[7; 32],
            "server",
            &["internal", &"a".repeat(64), &"b".repeat(64)],
        );
        assert_eq!(
            proof,
            "837967a87c8473a22583252dd47633c80d049b2c1157ac95bfaaa6464c7d2f92"
        );
        assert!(rpc_proof_matches(&proof, Some(&serde_json::json!(proof))));
    }
    #[test]
    fn mismatched_malformed_or_missing_proofs_never_match() {
        let proof = rpc_auth_proof(&[7; 32], "client", &["internal", "x", "y"]);
        assert!(!rpc_proof_matches(&proof, None));
        assert!(!rpc_proof_matches(&proof, Some(&serde_json::json!(1))));
        assert!(!rpc_proof_matches(
            &proof,
            Some(&serde_json::json!(proof.to_uppercase()))
        ));
        assert!(!rpc_proof_matches(
            &proof,
            Some(&serde_json::json!("00".repeat(32)))
        ));
    }
}
