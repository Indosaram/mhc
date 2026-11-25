//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-host-token.ts`.
//!
//! Host tokens of the sealed secret route (D11 secret phase, section 3). One in-memory Ed25519
//! host key per host instance, never persisted; one issuer token per `(hostInstance,
//! durableSessionId)` while a current grant on that session negotiated `secret_input_v1`,
//! re-minted at two thirds of its lifetime and dropped with the last such grant.

use crate::secret_sealed::{SecretBinding, secret_request_transcript};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};
use futures::future::BoxFuture;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HOST_TOKEN_AUDIENCE: &str = "omo-secret-host";
const HOST_TOKEN_TYPE: &str = "secret_host";
const MAX_LIFETIME_S: i64 = 900;
const SKEW_S: i64 = 60;
const MAX_TOKEN_CHARS: usize = 8192;
const MINT_TIMEOUT_MS: u64 = 30_000;
const RETRY_MAX_MS: u64 = 60_000;
const RETRY_MIN_MS: u64 = 5_000;
const RETRY_BEFORE_EXP_MS: i64 = 1_000;
const RETRY_MIN_WAIT_MS: u64 = 250;

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// RFC 7638/8037 thumbprint of an Ed25519 SPKI DER key (`presenterKeyThumbprint`).
pub fn presenter_key_thumbprint(spki_der: &[u8]) -> Option<String> {
    if spki_der.len() != 44 || !spki_der.starts_with(&ED25519_SPKI_PREFIX) {
        return None;
    }
    let x = URL_SAFE_NO_PAD.encode(&spki_der[12..]);
    let jwk = serde_json::to_string(&serde_json::json!({
        "crv": "Ed25519",
        "kty": "OKP",
        "x": x,
    }))
    .ok()?;
    Some(URL_SAFE_NO_PAD.encode(Sha256::digest(jwk.as_bytes())))
}

/// Display and cache inputs from the issuer: non-empty, at most 256 characters, no control characters.
pub fn bounded_claim(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 256 && !value.chars().any(char::is_control)
}

/// The issuer's half of the host-key proof (D11 secret phase 3.1).
#[derive(Debug, Clone)]
pub struct SecretHostProof {
    pub issuer: String,
    pub nonce: String,
}

pub type SecretProofSignFn =
    Arc<dyn Fn(&SecretHostProof) -> Result<String, &'static str> + Send + Sync>;

/// One host token mint for a session (D11 secret phase 3.1).
pub struct SecretHostTokenRequest {
    pub host_instance: String,
    pub durable_session_id: String,
    pub host_key_spki: Vec<u8>,
    pub sign: SecretProofSignFn,
}

/// Verified claims of a step-up token (D11 secret phase 6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAnswerStepUp {
    pub acc: String,
    pub dev: String,
    pub jti: String,
    pub exp: i64,
    pub hdev: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretAnswerRefusal {
    StepupMalformed,
    StepupSignature,
    StepupAudience,
    StepupExpired,
    StepupKeyMismatch,
    StepupBinding,
}

pub type SecretAnswerVerification = Result<SecretAnswerStepUp, SecretAnswerRefusal>;

/// Account authority surface required by [`SecretHostTokens`] and [`crate::secret_sealed_answers`].
pub trait SecretHostAuthority: Send + Sync {
    fn host_account(&self) -> Option<String>;
    fn can_mint_and_verify(&self) -> bool {
        true
    }
    fn mint_secret_host_token(
        &self,
        request: SecretHostTokenRequest,
    ) -> BoxFuture<'static, Result<String, String>>;
    fn verify_secret_answer(
        &self,
        token: String,
        presenter_spki: Vec<u8>,
        req: String,
        now_s: i64,
    ) -> BoxFuture<'static, SecretAnswerVerification>;
}

/// A host token the engine holds for one session, with the claims it decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHostToken {
    pub token: String,
    pub acc: String,
    /// The minting (host) device; a step-up token's `hdev` must equal it (6.5).
    pub dev: String,
    pub iat: i64,
    pub exp: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_s() -> i64 {
    now_ms() / 1000
}

fn claims_of(token: &str) -> Option<serde_json::Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.as_object().cloned()
}

pub type TokenListener = Arc<dyn Fn(&str, &SecretHostToken) + Send + Sync>;

struct Entry {
    binding_id: u64,
    held: Arc<dyn Fn() -> bool + Send + Sync>,
    token: Option<SecretHostToken>,
    refresh: Option<tokio::task::JoinHandle<()>>,
    minting: bool,
}

struct Inner {
    host_instance: String,
    authority: Arc<dyn SecretHostAuthority>,
    signing_key: SigningKey,
    spki: Vec<u8>,
    host_pk: String,
    host_jkt: String,
    entries: HashMap<String, Entry>,
    listeners: Vec<(u64, TokenListener)>,
    next_listener_id: u64,
    disposed: bool,
}

/// In-memory Ed25519 host key + cached per-session issuer tokens.
#[derive(Clone)]
pub struct SecretHostTokens {
    inner: Arc<Mutex<Inner>>,
}

impl SecretHostTokens {
    /// Returns `None` unless the authority can both mint host tokens and verify step-up tokens.
    pub fn create(
        host_instance: impl Into<String>,
        authority: Option<Arc<dyn SecretHostAuthority>>,
    ) -> Option<Self> {
        let authority = authority?;
        if !authority.can_mint_and_verify() {
            return None;
        }
        let mut seed = [0u8; 32];
        if getrandom::fill(&mut seed).is_err() {
            return None;
        }
        let signing_key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        let raw_pk = signing_key.verifying_key().to_bytes();
        let mut spki = Vec::with_capacity(44);
        spki.extend_from_slice(&ED25519_SPKI_PREFIX);
        spki.extend_from_slice(&raw_pk);
        let host_jkt = presenter_key_thumbprint(&spki)?;
        let host_pk = URL_SAFE_NO_PAD.encode(raw_pk);
        Some(Self {
            inner: Arc::new(Mutex::new(Inner {
                host_instance: host_instance.into(),
                authority,
                signing_key,
                spki,
                host_pk,
                host_jkt,
                entries: HashMap::new(),
                listeners: Vec::new(),
                next_listener_id: 1,
                disposed: false,
            })),
        })
    }

    pub fn host_pk(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host_pk
            .clone()
    }

    pub fn host_jkt(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host_jkt
            .clone()
    }

    /// A grant with `secret_input_v1` was acquired on `binding_id`. Mints the session's token
    /// unless a valid one is held.
    pub fn want(
        &self,
        durable_session_id: &str,
        binding_id: u64,
        held: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        if !held() {
            return;
        }
        let should_mint = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.disposed {
                return;
            }
            if inner
                .entries
                .get(durable_session_id)
                .is_some_and(|e| e.binding_id != binding_id)
                && let Some(mut old) = inner.entries.remove(durable_session_id)
                && let Some(h) = old.refresh.take()
            {
                h.abort();
            }
            let account = inner.authority.host_account();
            let entry = inner
                .entries
                .entry(durable_session_id.to_owned())
                .or_insert_with(|| Entry {
                    binding_id,
                    held,
                    token: None,
                    refresh: None,
                    minting: false,
                });
            let valid = Self::valid_for_account(entry.token.as_ref(), account.as_deref());
            !entry.minting && (entry.refresh.is_none() || !valid)
        };
        if should_mint {
            let this = self.clone();
            let sid = durable_session_id.to_owned();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    this.mint(&sid, binding_id).await;
                });
            }
        }
    }

    /// Notify that a binding's grants changed; drops the entry if `!held()`.
    pub fn notify_binding_changed(&self, durable_session_id: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_drop = inner
            .entries
            .get(durable_session_id)
            .is_some_and(|e| !(e.held)());
        if should_drop
            && let Some(mut old) = inner.entries.remove(durable_session_id)
            && let Some(h) = old.refresh.take()
        {
            h.abort();
        }
    }

    /// The session's token while it is valid now and still names the signed-in account.
    pub fn current(&self, durable_session_id: &str) -> Option<SecretHostToken> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = inner.entries.get(durable_session_id)?.token.as_ref()?;
        let account = inner.authority.host_account();
        Self::valid_for_account(Some(token), account.as_deref()).then(|| token.clone())
    }

    fn valid_for_account(token: Option<&SecretHostToken>, account: Option<&str>) -> bool {
        let Some(token) = token else {
            return false;
        };
        let Some(acc) = account else {
            return false;
        };
        now_ms() < token.exp.saturating_mul(1000) && token.acc == acc
    }

    /// The host key's signature over a request binding, as 128 lowercase hex characters (0022 3.5).
    pub fn sign_request(&self, binding: &SecretBinding) -> Result<String, &'static str> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if binding.1 != inner.host_instance || binding.2 != inner.host_jkt {
            return Err("secret_request_foreign");
        }
        let transcript = secret_request_transcript(binding);
        let sig = inner.signing_key.sign(transcript.as_bytes());
        Ok(hex::encode(sig.to_bytes()))
    }

    /// Fires after every successful mint or re-mint; returns an unsubscribe handle.
    pub fn on_token(&self, listener: TokenListener) -> Box<dyn FnOnce() + Send> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = inner.next_listener_id;
        inner.next_listener_id += 1;
        inner.listeners.push((id, listener));
        let weak = Arc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(arc) = weak.upgrade() {
                let mut inner = arc
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                inner.listeners.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    pub fn dispose(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.listeners.clear();
        inner.disposed = true;
        for (_, mut entry) in inner.entries.drain() {
            if let Some(h) = entry.refresh.take() {
                h.abort();
            }
        }
    }

    /// Synchronous mint helper usable inside tests or async callers.
    pub async fn mint(&self, durable_session_id: &str, binding_id: u64) {
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (host_instance, host_jkt, spki, signing_key, authority, has_account) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.disposed {
                return;
            }
            let Some(entry) = inner.entries.get_mut(durable_session_id) else {
                return;
            };
            if entry.binding_id != binding_id {
                return;
            }
            if let Some(h) = entry.refresh.take() {
                h.abort();
            }
            entry.minting = true;
            (
                inner.host_instance.clone(),
                inner.host_jkt.clone(),
                inner.spki.clone(),
                inner.signing_key.clone(),
                inner.authority.clone(),
                inner.authority.host_account().is_some(),
            )
        };

        let sid = durable_session_id.to_owned();
        let hin = host_instance.clone();
        let jkt = host_jkt.clone();
        let open_flag = open.clone();
        let proof_fn: SecretProofSignFn = Arc::new(move |input: &SecretHostProof| {
            if !open_flag.load(std::sync::atomic::Ordering::Acquire)
                || !bounded_claim(&input.issuer)
                || !bounded_claim(&input.nonce)
            {
                return Err("secret_host_proof_refused");
            }
            let transcript = [
                "omo-secret-host-proof/v1",
                input.issuer.as_str(),
                HOST_TOKEN_AUDIENCE,
                jkt.as_str(),
                input.nonce.as_str(),
                hin.as_str(),
                sid.as_str(),
            ]
            .join("\n");
            let sig = signing_key.sign(transcript.as_bytes());
            Ok(URL_SAFE_NO_PAD.encode(sig.to_bytes()))
        });

        let minted = if has_account {
            let req = SecretHostTokenRequest {
                host_instance: host_instance.clone(),
                durable_session_id: durable_session_id.to_owned(),
                host_key_spki: spki,
                sign: proof_fn,
            };
            tokio::time::timeout(
                Duration::from_millis(MINT_TIMEOUT_MS),
                authority.mint_secret_host_token(req),
            )
            .await
            .ok()
            .and_then(Result::ok)
        } else {
            None
        };
        open.store(false, std::sync::atomic::Ordering::Release);

        let (accepted_token, listeners_to_fire, retry_delay_ms) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let account = inner.authority.host_account();
            let host_jkt = inner.host_jkt.clone();
            let host_instance = inner.host_instance.clone();
            let disposed = inner.disposed;
            let Some(entry) = inner.entries.get_mut(durable_session_id) else {
                return;
            };
            entry.minting = false;
            if disposed || entry.binding_id != binding_id {
                return;
            }
            let accepted = minted.as_deref().and_then(|t| {
                Self::accept_claims(
                    t,
                    durable_session_id,
                    &host_instance,
                    &host_jkt,
                    account.as_deref(),
                )
            });
            if let Some(token) = accepted {
                entry.token = Some(token.clone());
                let delay = ((token.exp.saturating_mul(1000) - now_ms()).max(0) as u64 * 2) / 3;
                let listeners: Vec<TokenListener> =
                    inner.listeners.iter().map(|(_, l)| l.clone()).collect();
                (Some(token), listeners, delay)
            } else {
                if !(entry.held)() {
                    return;
                }
                let remaining_ms =
                    if Self::valid_for_account(entry.token.as_ref(), account.as_deref()) {
                        entry
                            .token
                            .as_ref()
                            .map(|t| t.exp.saturating_mul(1000) - now_ms())
                    } else {
                        None
                    };
                let delay = match remaining_ms {
                    None => RETRY_MAX_MS,
                    Some(rem) => {
                        let left = (rem.max(0) as u64) / 3;
                        let clamped = left.clamp(RETRY_MIN_MS, RETRY_MAX_MS);
                        let cap = (rem - RETRY_BEFORE_EXP_MS).max(RETRY_MIN_WAIT_MS as i64) as u64;
                        clamped.min(cap)
                    }
                };
                (None, Vec::new(), delay)
            }
        };

        if let Some(token) = accepted_token {
            for listener in listeners_to_fire {
                listener(durable_session_id, &token);
            }
        }
        self.arm(durable_session_id, binding_id, retry_delay_ms);
    }

    fn arm(&self, durable_session_id: &str, binding_id: u64, delay_ms: u64) {
        let this = self.clone();
        let sid = durable_session_id.to_owned();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let handle = runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let should_run = {
                let mut inner = this
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if inner.disposed {
                    false
                } else if let Some(entry) = inner.entries.get_mut(&sid) {
                    entry.refresh = None;
                    entry.binding_id == binding_id && !entry.minting
                } else {
                    false
                }
            };
            if should_run {
                this.mint(&sid, binding_id).await;
            }
        });
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = inner.entries.get_mut(durable_session_id)
            && entry.binding_id == binding_id
        {
            entry.refresh = Some(handle);
        } else {
            handle.abort();
        }
    }

    pub fn accept_claims(
        token: &str,
        durable_session_id: &str,
        host_instance: &str,
        host_jkt: &str,
        host_account: Option<&str>,
    ) -> Option<SecretHostToken> {
        if token.is_empty() || token.len() > MAX_TOKEN_CHARS {
            return None;
        }
        let host_acc = host_account?;
        let claims = claims_of(token)?;
        let cnf = claims.get("cnf")?.as_object()?;
        if claims.get("aud").and_then(Value::as_str) != Some(HOST_TOKEN_AUDIENCE)
            || claims.get("typ").and_then(Value::as_str) != Some(HOST_TOKEN_TYPE)
            || claims.get("hin").and_then(Value::as_str) != Some(host_instance)
            || claims.get("sid").and_then(Value::as_str) != Some(durable_session_id)
            || cnf.get("jkt").and_then(Value::as_str) != Some(host_jkt)
        {
            return None;
        }
        let acc = claims.get("acc").and_then(Value::as_str)?;
        if acc != host_acc {
            return None;
        }
        let dev = claims.get("dev").and_then(Value::as_str)?;
        if !bounded_claim(dev) {
            return None;
        }
        let iat = claims.get("iat").and_then(Value::as_i64)?;
        let exp = claims.get("exp").and_then(Value::as_i64)?;
        let now = now_s();
        if exp <= iat || exp - iat > MAX_LIFETIME_S || iat > now + SKEW_S || now >= exp {
            return None;
        }
        Some(SecretHostToken {
            token: token.to_owned(),
            acc: acc.to_owned(),
            dev: dev.to_owned(),
            iat,
            exp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_sealed::{SecretBindingInput, secret_binding};

    struct TestAuthority {
        acc: Mutex<Option<String>>,
        next_exp_offset: i64,
    }

    impl SecretHostAuthority for TestAuthority {
        fn host_account(&self) -> Option<String> {
            self.acc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn mint_secret_host_token(
            &self,
            request: SecretHostTokenRequest,
        ) -> BoxFuture<'static, Result<String, String>> {
            let exp_offset = self.next_exp_offset;
            let acc = self.host_account().unwrap_or_default();
            Box::pin(async move {
                let proof = (request.sign)(&SecretHostProof {
                    issuer: "https://issuer.test".into(),
                    nonce: "n1".into(),
                })
                .map_err(|e| e.to_owned())?;
                assert!(!proof.is_empty());
                let jkt = presenter_key_thumbprint(&request.host_key_spki).expect("jkt");
                let now = now_s();
                let claims = serde_json::json!({
                    "aud": HOST_TOKEN_AUDIENCE,
                    "typ": HOST_TOKEN_TYPE,
                    "hin": request.host_instance,
                    "sid": request.durable_session_id,
                    "cnf": { "jkt": jkt },
                    "acc": acc,
                    "dev": "host-dev-1",
                    "iat": now,
                    "exp": now + exp_offset,
                });
                let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA"}"#);
                let pay = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
                Ok(format!("{hdr}.{pay}.sig"))
            })
        }

        fn verify_secret_answer(
            &self,
            _token: String,
            _presenter_spki: Vec<u8>,
            _req: String,
            _now_s: i64,
        ) -> BoxFuture<'static, SecretAnswerVerification> {
            Box::pin(async { Err(SecretAnswerRefusal::StepupMalformed) })
        }
    }

    #[tokio::test]
    async fn mints_caches_signs_and_rejects_foreign_bindings() {
        let auth = Arc::new(TestAuthority {
            acc: Mutex::new(Some("acc-1".into())),
            next_exp_offset: 300,
        });
        let tokens = SecretHostTokens::create("hin-1", Some(auth.clone())).expect("tokens");
        let (tx, rx) = tokio::sync::oneshot::channel::<SecretHostToken>();
        let tx = Mutex::new(Some(tx));
        let _unsub = tokens.on_token(Arc::new(move |sid, tok| {
            assert_eq!(sid, "dur-1");
            if let Some(sender) = tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(tok.clone());
            }
        }));
        tokens.want("dur-1", 1, Arc::new(|| true));
        let minted = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("mint timeout")
            .expect("minted");
        assert_eq!(minted.acc, "acc-1");
        assert_eq!(tokens.current("dur-1").expect("cached").dev, "host-dev-1");

        // Account change invalidates cached token.
        *auth
            .acc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some("acc-2".into());
        assert!(tokens.current("dur-1").is_none());

        // Sign binding for this host succeeds, foreign host rejected.
        let b = secret_binding(&SecretBindingInput {
            host_instance: "hin-1".into(),
            host_jkt: tokens.host_jkt(),
            durable_session_id: "dur-1".into(),
            ui_request_id: "u1".into(),
            deadline_at_ms: 1_700_000_000_000,
            hpke_pk: tokens.host_pk(),
            title: "t".into(),
            placeholder: None,
        })
        .expect("binding");
        assert_eq!(tokens.sign_request(&b).expect("sig").len(), 128);
        let mut foreign = b;
        foreign.1 = "other-host".into();
        assert_eq!(tokens.sign_request(&foreign), Err("secret_request_foreign"));
        tokens.dispose();
    }
}
