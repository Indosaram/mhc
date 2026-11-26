//! ID-token cases from packages/ai/test/openai-chatgpt-oauth.test.ts.

use super::*;
use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
use crate::utils::abort::AbortController;
use base64::engine::general_purpose::STANDARD;
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use serde_json::json;

// Synthetic fixture generated solely to sign arbitrary unit-test claims.
const TEST_KEY: &str = concat!(
    "MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCuGYuRU4TMdWsqu2AtkqDdez+eM/s2avv0vYfrG48uCRpr9yVQ",
    "rXdgYgbNmJGZAtSjYW1kaS3jQY7ibX62ONfviwf3ZFUo6n0LqG6n2ylcZp1Ia1ZvfoIJRA0TolLbCadws+Imqu3pj4EeIRsPR8xz",
    "+qmJOqCwd+vMgNADjg3WRevKI5oaG+J1KLjlgY/0lecbmwZbaPbpxLp1Ph4AMOaFpvewI3euYTDV+f6IxwuXyMj16TQsSnAZHlHt",
    "c81zcrJKuav/wQiQtZQMM8v+G+jQrGUUtuXBHL9e6VLYVmrGESawGhbOnG3iYOkKDQBjUINW9pOhthmWt2+cjz4U1ulrAgMBAAEC",
    "ggEAR/gIYWFwlqJ9VBNULVKdTCE8SbY8RSERmXjV3GMum32o3fOzCbhDjmTHYisNGP76DrXiJi1yiiyaF9Dingo+fWfwOZzQtVLF",
    "sVGFuL7YyLkkkIDKoNlfgNwQOcsSzDgf7u91b72eDrxDSESwLm9kPZdjiPZi4cAZChc/phCNeMMNK9rsDxL8RVBaF1FUF17TQdXy",
    "g+Uq4/8ivHpiHI9Q6kuIq0nWo5RPi46Qp1XRFq3OeRlhlE4TwNFhIxAch++2KopCODViAwpAim8arqVPBChYx1i/m5YdhwREZnNQ",
    "1T5r0rDAO32Mq2KMKSpP7lXzDLL2xurhrx/Sv5QMb8im4QKBgQDT6hwqK6Dw/UufLleHBC2HJip6VHaE7hsG+4Z946hwMlrrbFJk",
    "V6FXlldZSEtNkAmcuIg4654z0yaZLuVZ9ZljyyNXw7U3WPEJ30Rg368rDZSNeVVQZCsoFqzRbsaAzKvziZeLjzHKBZW6hLq8q0bt",
    "E96MJQYGD6WP9q0F1ucFiwKBgQDSUYtlHxVHKZtY6Dn+9DAzikCjvywF14clnUiu57YVnSUiqDZf2haA8wl3widiC2eabkf5/BF4",
    "vwEoSUu32m6uxZW1KHAEaTgPATrot/u4L+22YEi1S1n39VVeKe/aBKqGbqMPbKmiJeSD6d0kbirdQ4+9bF8u6EkorKC2ECTnoQKB",
    "gCIEOnEqf7MvdawfrD0oFobTQY5RcJT2A7CjP3uFx8v6yU1xHeitiGCe6XaTfEL/FAvsMzGv4L1A77k3JenC2tSCDzJPYA96X3bO",
    "OBO01FF85JOxHljBO4w715HmzkZulEuLkp/8ZGmha9983tzaQZBc2CR27PicGl95oVyGmp7xAoGAafDReppxqWr36YzwydBPB2c4",
    "sEL39elfFj9oAVKOkcyajB6XZSkB9Op3dm/CTszX3x51+yJR2rS77IMfNPk+j6LPinVYF6C4sDY6akdi8KCT6S4O8eOG6jQWUFxn",
    "iTaYfybz1C00NY1I0nnW/ybPSK1jllMkjwhK8cYjHhJ7xIECgYBxj1jM6KtNu6z1zFFGSEMCOLbhyHFAyF+s40WKMzOY4RiHJKqB",
    "uKq57YfbeKrZpIqIJ4atQWUymNlKdj8jv9GLM4qd8nj3xPxNS+m4Pl0ejXXRZaJAIYy/KvDKSzApt/m5BOSHyyryH9CToBSyIHn8",
    "D9awR37BpbLeUPcrOR6Cqg==",
);
const MODULUS: &str = concat!(
    "rhmLkVOEzHVrKrtgLZKg3Xs_njP7Nmr79L2H6xuPLgkaa_clUK13YGIGzZiRmQLUo2FtZGkt40GO4m1-tjjX74sH92RVKOp9C6",
    "hup9spXGadSGtWb36CCUQNE6JS2wmncLPiJqrt6Y-BHiEbD0fMc_qpiTqgsHfrzIDQA44N1kXryiOaGhvidSi45YGP9JXnG5s",
    "GW2j26cS6dT4eADDmhab3sCN3rmEw1fn-iMcLl8jI9ek0LEpwGR5R7XPNc3KySrmr_8EIkLWUDDPL_hvo0KxlFLblwRy_XulS2",
    "FZqxhEmsBoWzpxt4mDpCg0AY1CDVvaTobYZlrdvnI8-FNbpaw",
);

pub(in crate::auth::oauth) fn claims(client: &str, nonce: &str) -> Value {
    json!({"iss":ISSUER,"aud":client,"exp":4_102_444_800_u64,"nonce":nonce,
           "sub":"test-subject","email":"test@example.test"})
}

pub(in crate::auth::oauth) fn signed_token(claims: Value, header: Value) -> String {
    let key = RsaKeyPair::from_pkcs8(&STANDARD.decode(TEST_KEY).unwrap()).unwrap();
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string()),
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
}

pub(in crate::auth::oauth) fn discovery() -> ScriptedResponse {
    response(200, json!({"jwks_uri":"https://example.test/jwks"}))
}

pub(in crate::auth::oauth) fn jwks(kid: &str) -> ScriptedResponse {
    response(200, json!({"keys":[key(kid)]}))
}

fn key(kid: &str) -> Value {
    json!({"kid":kid,"kty":"RSA","alg":"RS256","use":"sig","n":MODULUS,"e":"AQAB"})
}

fn response(status: u16, body: Value) -> ScriptedResponse {
    ScriptedResponse::Json { status, body }
}

fn test_token(claims: Value, kid: &str) -> String {
    signed_token(claims, json!({"alg":"RS256","kid":kid}))
}

fn transport() -> ScriptedTransport {
    let transport = ScriptedTransport::new(vec![]);
    transport.set_now_ms(1000.0);
    transport
}

#[tokio::test]
async fn rejects_invalid_signatures_and_identity_claims() {
    let transport = transport();
    transport.push(Some("openid-configuration"), discovery());
    transport.push(Some("/jwks"), jwks("test-key"));
    let verifier = OpenAIIdTokenVerifier::default();
    let signal = AbortController::new().signal();
    for (claim, value, expected) in [
        (
            "iss",
            json!("https://other.example.test"),
            "issuer mismatch",
        ),
        ("aud", json!("other"), "audience mismatch"),
        ("aud", json!(["other"]), "audience mismatch"),
        ("aud", json!(["client", 0]), "audience mismatch"),
        ("exp", json!(0), "expired or missing expiration"),
        ("exp", Value::Null, "expired or missing expiration"),
        ("nonce", json!("another-attempt"), "nonce mismatch"),
        ("sub", json!(""), "missing subject"),
    ] {
        let mut claims = claims("client", "nonce");
        if value.is_null() {
            claims.as_object_mut().unwrap().remove(claim);
        } else {
            claims[claim] = value;
        }
        let token = test_token(claims, "test-key");
        let error = verifier
            .verify(&transport, Some(&token), "client", "nonce", &signal)
            .await
            .unwrap_err();
        assert_eq!(error.0, expected, "{claim}");
    }
    let token = test_token(claims("client", "nonce"), "test-key");
    let (input, signature) = token.rsplit_once('.').unwrap();
    let mut bytes = decode(signature).unwrap();
    bytes[0] ^= 1;
    let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(bytes));
    let error = verifier
        .verify(&transport, Some(&token), "client", "nonce", &signal)
        .await
        .unwrap_err();
    assert_eq!(error.0, "invalid signature");
}

#[tokio::test]
async fn uses_injected_clock_accepts_audience_arrays_and_expires_both_caches() {
    let transport = transport();
    let signal = AbortController::new().signal();
    let verifier = OpenAIIdTokenVerifier::default();
    for _ in 0..2 {
        transport.push(Some("openid-configuration"), discovery());
        transport.push(Some("/jwks"), jwks("test-key"));
    }
    let mut claims = claims("client", "nonce");
    claims["aud"] = json!(["other", "client"]);
    let token = test_token(claims, "test-key");
    for expected_requests in [2, 2, 4] {
        let identity = verifier
            .verify(&transport, Some(&token), "client", "nonce", &signal)
            .await
            .unwrap();
        assert_eq!(identity.subject, "test-subject");
        assert_eq!(identity.issuer, ISSUER);
        assert_eq!(identity.email.as_deref(), Some("test@example.test"));
        assert_eq!(transport.requests().len(), expected_requests);
        transport.advance_ms(CACHE_MS / 2.0);
    }
}

#[tokio::test]
async fn rejects_expiration_at_the_injected_clock_boundary() {
    let transport = transport();
    transport.push(Some("openid-configuration"), discovery());
    transport.push(Some("/jwks"), jwks("test-key"));
    let mut claims = claims("client", "nonce");
    claims["exp"] = json!(1);
    let token = test_token(claims, "test-key");
    let error = OpenAIIdTokenVerifier::default()
        .verify(
            &transport,
            Some(&token),
            "client",
            "nonce",
            &AbortController::new().signal(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0, "expired or missing expiration");
}

#[tokio::test]
async fn reloads_jwks_once_for_an_unfamiliar_signing_key() {
    let transport = transport();
    transport.push(Some("openid-configuration"), discovery());
    transport.push(Some("/jwks"), jwks("old-key"));
    transport.push(Some("/jwks"), jwks("test-key"));
    let signal = AbortController::new().signal();
    let verifier = OpenAIIdTokenVerifier::default();
    for kid in ["old-key", "test-key"] {
        let token = test_token(claims("client", "nonce"), kid);
        assert!(
            verifier
                .verify(&transport, Some(&token), "client", "nonce", &signal)
                .await
                .is_ok()
        );
    }
    assert_eq!(
        transport
            .requests()
            .iter()
            .map(|r| r.url.as_str())
            .collect::<Vec<_>>(),
        [
            DISCOVERY_URL,
            "https://example.test/jwks",
            "https://example.test/jwks"
        ]
    );
}

#[tokio::test]
async fn fails_closed_on_discovery_jwks_unknown_or_ambiguous_signing_keys() {
    for (responses, expected, count) in [
        (
            vec![response(503, json!({}))],
            "discovery request failed",
            1,
        ),
        (
            vec![discovery(), response(503, json!({}))],
            "JWKS request failed",
            2,
        ),
        (
            vec![discovery(), jwks("unknown"), jwks("unknown")],
            "signing key not found or ambiguous",
            3,
        ),
        (
            vec![
                discovery(),
                response(200, json!({"keys":[key("test-key"), key("test-key")]})),
            ],
            "signing key not found or ambiguous",
            2,
        ),
        (
            vec![response(
                200,
                json!({"jwks_uri":"http://example.test/jwks"}),
            )],
            "invalid discovery JWKS URI",
            1,
        ),
    ] {
        let transport = ScriptedTransport::new(
            responses
                .into_iter()
                .map(|response| (None, response))
                .collect(),
        );
        let token = test_token(claims("client", "nonce"), "test-key");
        let error = OpenAIIdTokenVerifier::default()
            .verify(
                &transport,
                Some(&token),
                "client",
                "nonce",
                &AbortController::new().signal(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.0, expected);
        assert_eq!(transport.requests().len(), count);
    }
}

#[tokio::test]
async fn rejects_malformed_tokens_and_unsupported_headers_before_discovery() {
    let transport = transport();
    let signal = AbortController::new().signal();
    let verifier = OpenAIIdTokenVerifier::default();
    for token in [
        None,
        Some(""),
        Some("not-a-jwt"),
        Some("e30.e30.e30"),
        Some("%%%.e30.e30"),
    ] {
        assert!(
            verifier
                .verify(&transport, token, "client", "nonce", &signal)
                .await
                .is_err()
        );
    }
    for header in [
        json!({"alg":"none","kid":"test-key"}),
        json!({"alg":"RS256","kid":"test-key","crit":null}),
    ] {
        let token = signed_token(claims("client", "nonce"), header);
        let error = verifier
            .verify(&transport, Some(&token), "client", "nonce", &signal)
            .await
            .unwrap_err();
        assert_eq!(error.0, "unsupported signing header");
    }
    assert!(transport.requests().is_empty());
}
