//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-sealed-answers.ts`.
//!
//! Design 7.1 steps 1, 4 and 5 for `mcp_presenter_secret_answer`, in the process that holds the
//! authority, plus step 6's `open_sealed_value`.

use crate::ed25519_strict::strict_ed25519_verify;
use crate::secret_host_token::{SecretHostAuthority, SecretHostToken};
use crate::secret_input_requests::SealedAnswer;
use crate::secret_sealed::{
    secret_answer_aad, secret_answer_info, secret_answer_transcript, secret_cancel_transcript,
    unpad_secret_value,
};
use crate::secret_sealed_hpke::{SecretOpenInput, X25519Dh, open_secret_answer};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::VerifyingKey;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

const LINE_ID_MAX: usize = 128;
const CT_MIN: usize = 256 + 16;
const CT_MAX: usize = 65536 + 16;
const VALUE_KEYS: [&str; 10] = [
    "type",
    "id",
    "v",
    "grantId",
    "uiRequestId",
    "answerId",
    "stepup",
    "enc",
    "ct",
    "sig",
];
const CANCEL_KEYS: [&str; 8] = [
    "type",
    "id",
    "v",
    "grantId",
    "uiRequestId",
    "answerId",
    "cancelled",
    "sig",
];
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedAnswerValueFrame {
    pub stepup: String,
    pub enc: Vec<u8>,
    pub ct: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedAnswerFrame {
    pub id: String,
    pub grant_id: String,
    pub ui_request_id: String,
    pub answer_id: String,
    pub sig: Vec<u8>,
    pub value: Option<SealedAnswerValueFrame>,
}

/// Account-enrolled presenter principal (`AccountPrincipal` in `mcp-presenter-account.ts`).
#[derive(Debug, Clone)]
pub struct AccountPrincipal {
    pub id: String,
    pub key: VerifyingKey,
    pub acc: String,
    pub dev: String,
    pub dname: String,
    pub installation_key_id: String,
    pub attested_until: i64,
}

impl AccountPrincipal {
    pub fn spki_der(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(44);
        out.extend_from_slice(&ED25519_SPKI_PREFIX);
        out.extend_from_slice(self.key.as_bytes());
        out
    }
}

pub struct SealedAnswerContext<'a> {
    pub frame: &'a SealedAnswerFrame,
    pub principal: &'a AccountPrincipal,
    pub req: &'a str,
    pub deadline_at_ms: i64,
    pub host_token: Option<&'a SecretHostToken>,
    pub account: &'a dyn SecretHostAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedSealedError {
    InvalidSecretResponse,
    SecretStepupRefused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedSealedAnswer {
    Ok {
        jti: Option<String>,
        exp: Option<i64>,
    },
    Err(CheckedSealedError),
}

fn bounded(value: Option<&Value>, max: usize) -> Option<String> {
    let s = value?.as_str()?;
    let len = s.encode_utf16().count();
    (len >= 1 && len <= max).then(|| s.to_owned())
}

fn canonical_b64url(value: Option<&Value>, min: usize, max: usize) -> Option<Vec<u8>> {
    let s = value?.as_str()?;
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
    (bytes.len() >= min && bytes.len() <= max && URL_SAFE_NO_PAD.encode(&bytes) == s)
        .then_some(bytes)
}

/// Step 1: the exact key set, `v === 1` and every bound; `None` is `invalid_secret_response`.
pub fn parse_sealed_answer(value: &Value) -> Option<SealedAnswerFrame> {
    let obj = value.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("mcp_presenter_secret_answer")
        || obj.get("v").and_then(Value::as_u64) != Some(1)
    {
        return None;
    }
    let cancel = obj.get("cancelled").and_then(Value::as_bool) == Some(true);
    let expected: BTreeSet<&str> = if cancel {
        CANCEL_KEYS.into_iter().collect()
    } else {
        VALUE_KEYS.into_iter().collect()
    };
    let actual: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    if actual != expected {
        return None;
    }
    let id = bounded(obj.get("id"), LINE_ID_MAX)?;
    let grant_id = bounded(obj.get("grantId"), 256)?;
    let ui_request_id = bounded(obj.get("uiRequestId"), 256)?;
    let answer_id = bounded(obj.get("answerId"), 256)?;
    let sig_hex = obj.get("sig").and_then(Value::as_str)?;
    if sig_hex.len() != 128
        || !sig_hex
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let sig = hex::decode(sig_hex).ok()?;
    if cancel {
        return Some(SealedAnswerFrame {
            id,
            grant_id,
            ui_request_id,
            answer_id,
            sig,
            value: None,
        });
    }
    let enc = canonical_b64url(obj.get("enc"), 32, 32)?;
    let ct = canonical_b64url(obj.get("ct"), CT_MIN, CT_MAX)?;
    let stepup = bounded(obj.get("stepup"), 8192)?;
    Some(SealedAnswerFrame {
        id,
        grant_id,
        ui_request_id,
        answer_id,
        sig,
        value: Some(SealedAnswerValueFrame { stepup, enc, ct }),
    })
}

fn jti_of(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    bounded(claims.as_object()?.get("jti"), 256)
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Step 4: the envelope signature under the principal's installation key, verified strictly.
/// Step 5 for a value: `verify_secret_answer` and the 6.5 host checks.
pub async fn check_sealed_answer(context: &SealedAnswerContext<'_>) -> CheckedSealedAnswer {
    let frame = context.frame;
    let principal = context.principal;
    let req = context.req;
    let jti = match &frame.value {
        Some(v) => match jti_of(&v.stepup) {
            Some(jti) => Some(jti),
            None => return CheckedSealedAnswer::Err(CheckedSealedError::InvalidSecretResponse),
        },
        None => None,
    };
    let transcript = match (&frame.value, &jti) {
        (Some(v), Some(jti)) => secret_answer_transcript(
            req,
            &principal.id,
            &frame.grant_id,
            &frame.answer_id,
            jti,
            &v.enc,
            &v.ct,
        ),
        _ => secret_cancel_transcript(req, &principal.id, &frame.grant_id, &frame.answer_id),
    };
    if !strict_ed25519_verify(principal.key.as_bytes(), transcript.as_bytes(), &frame.sig) {
        return CheckedSealedAnswer::Err(CheckedSealedError::InvalidSecretResponse);
    }
    let Some(value) = &frame.value else {
        return CheckedSealedAnswer::Ok {
            jti: None,
            exp: None,
        };
    };
    let Some(jti) = jti else {
        return CheckedSealedAnswer::Err(CheckedSealedError::InvalidSecretResponse);
    };
    let spki = principal.spki_der();
    let result = context
        .account
        .verify_secret_answer(value.stepup.clone(), spki, req.to_owned(), now_s())
        .await;
    let Ok(step_up) = result else {
        return CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused);
    };
    let Some(host) = context.account.host_account() else {
        return CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused);
    };
    let Some(host_token) = context.host_token else {
        return CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused);
    };
    const MAX_SAFE_INT: i64 = (1i64 << 53) - 1;
    if step_up.jti != jti
        || step_up.acc != host
        || step_up.acc != principal.acc
        || step_up.dev != principal.dev
        || step_up.hdev != host_token.dev
        || step_up.exp.abs() > MAX_SAFE_INT
        || step_up.exp.saturating_mul(1000) > context.deadline_at_ms
    {
        return CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused);
    }
    CheckedSealedAnswer::Ok {
        jti: Some(jti),
        exp: Some(step_up.exp),
    }
}

/// Step 6's open, run at the latch with the request key: HPKE open, then the canonical unpad.
pub fn open_sealed_value(
    answer: &SealedAnswer,
    private_key: &[u8],
    hpke_pk: &str,
    dh: &dyn X25519Dh,
) -> Option<String> {
    let val = answer.value.as_ref()?;
    let pk_bytes = URL_SAFE_NO_PAD.decode(hpke_pk).ok()?;
    let enc = URL_SAFE_NO_PAD.decode(&val.enc).ok()?;
    let ct = URL_SAFE_NO_PAD.decode(&val.ct).ok()?;
    let info = secret_answer_info(&answer.req);
    let aad = secret_answer_aad(
        &answer.req,
        &answer.principal,
        &answer.grant_id,
        &answer.answer_id,
        &val.jti,
    );
    let mut plaintext = open_secret_answer(
        &SecretOpenInput {
            private_key,
            recipient_public_key: &pk_bytes,
            enc: &enc,
            ct: &ct,
            info: &info,
            aad: &aad,
        },
        dh,
    )?;
    let value = unpad_secret_value(&plaintext);
    plaintext.fill(0);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_host_token::{SecretAnswerStepUp, SecretHostTokenRequest};
    use ed25519_dalek::{Signer, SigningKey};
    use futures::future::BoxFuture;
    use serde_json::json;

    #[test]
    fn parse_sealed_answer_enforces_exact_keys_and_bounds() {
        let sig = "a".repeat(128);
        let cancel = json!({
            "type": "mcp_presenter_secret_answer",
            "id": "line-1",
            "v": 1,
            "grantId": "g-1",
            "uiRequestId": "u-1",
            "answerId": "a-1",
            "cancelled": true,
            "sig": sig,
        });
        assert!(parse_sealed_answer(&cancel).is_some());

        let mut extra = cancel.clone();
        extra["extra"] = json!(1);
        assert!(parse_sealed_answer(&extra).is_none());

        let enc = URL_SAFE_NO_PAD.encode([3u8; 32]);
        let ct = URL_SAFE_NO_PAD.encode(vec![4u8; CT_MIN]);
        let val = json!({
            "type": "mcp_presenter_secret_answer",
            "id": "line-1",
            "v": 1,
            "grantId": "g-1",
            "uiRequestId": "u-1",
            "answerId": "a-1",
            "stepup": "hdr.pay.sig",
            "enc": enc,
            "ct": ct,
            "sig": sig,
        });
        let parsed = parse_sealed_answer(&val).expect("valid value frame");
        assert!(parsed.value.is_some());
    }

    struct MockAccount;
    impl SecretHostAuthority for MockAccount {
        fn host_account(&self) -> Option<String> {
            Some("acc-1".into())
        }
        fn mint_secret_host_token(
            &self,
            _: SecretHostTokenRequest,
        ) -> BoxFuture<'static, Result<String, String>> {
            Box::pin(async { Ok(String::new()) })
        }
        fn verify_secret_answer(
            &self,
            _: String,
            _: Vec<u8>,
            _: String,
            _: i64,
        ) -> BoxFuture<'static, crate::secret_host_token::SecretAnswerVerification> {
            Box::pin(async {
                Ok(SecretAnswerStepUp {
                    acc: "acc-1".into(),
                    dev: "dev-1".into(),
                    jti: "jti-1".into(),
                    exp: 2_000_000_000,
                    hdev: "host-dev-1".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn check_sealed_answer_verifies_cancel_and_stepup_binding() {
        let sk = SigningKey::from_bytes(&[11u8; 32]);
        let principal = AccountPrincipal {
            id: "p-1".into(),
            key: sk.verifying_key(),
            acc: "acc-1".into(),
            dev: "dev-1".into(),
            dname: "Phone".into(),
            installation_key_id: "ik-1".into(),
            attested_until: 2_000_000_000,
        };
        let tx = secret_cancel_transcript("req-1", "p-1", "g-1", "a-1");
        let sig = sk.sign(tx.as_bytes()).to_bytes().to_vec();
        let frame = SealedAnswerFrame {
            id: "l1".into(),
            grant_id: "g-1".into(),
            ui_request_id: "u-1".into(),
            answer_id: "a-1".into(),
            sig,
            value: None,
        };
        let res = check_sealed_answer(&SealedAnswerContext {
            frame: &frame,
            principal: &principal,
            req: "req-1",
            deadline_at_ms: 2_100_000_000_000,
            host_token: None,
            account: &MockAccount,
        })
        .await;
        assert_eq!(
            res,
            CheckedSealedAnswer::Ok {
                jti: None,
                exp: None
            }
        );

        // Value answer whose stepup exp exceeds deadline is refused.
        let pay = URL_SAFE_NO_PAD.encode(br#"{"jti":"jti-1"}"#);
        let stepup = format!("h.{pay}.s");
        let enc = vec![3u8; 32];
        let ct = vec![4u8; CT_MIN];
        let vtx = secret_answer_transcript("req-1", "p-1", "g-1", "a-1", "jti-1", &enc, &ct);
        let vsig = sk.sign(vtx.as_bytes()).to_bytes().to_vec();
        let vframe = SealedAnswerFrame {
            id: "l1".into(),
            grant_id: "g-1".into(),
            ui_request_id: "u-1".into(),
            answer_id: "a-1".into(),
            sig: vsig,
            value: Some(SealedAnswerValueFrame { stepup, enc, ct }),
        };
        let ht = SecretHostToken {
            token: "ht".into(),
            acc: "acc-1".into(),
            dev: "host-dev-1".into(),
            iat: 100,
            exp: 900,
        };
        let refused = check_sealed_answer(&SealedAnswerContext {
            frame: &vframe,
            principal: &principal,
            req: "req-1",
            deadline_at_ms: 1_900_000_000_000, // before step_up.exp * 1000
            host_token: Some(&ht),
            account: &MockAccount,
        })
        .await;
        assert_eq!(
            refused,
            CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused)
        );
    }
}
