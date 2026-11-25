//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-sealed-holders.ts`.
//!
//! The presenter control's sealed holders: eligibility, signed requests and token refreshes.

use crate::secret_host_token::{SecretHostAuthority, SecretHostTokens};
use crate::secret_input_requests::{
    SealedAnswer, SealedAnswerError, SealedAnswerReply, SealedAnswerValue,
};
use crate::secret_sealed::{
    McpPresenterSecretFrame, McpPresenterSecretRequest, McpPresenterSecretSettled,
    McpPresenterSecretToken, SecretBindingInput, SecretInputOutcome, secret_binding,
    secret_request_hash,
};
use crate::secret_sealed_answers::{
    AccountPrincipal, CheckedSealedAnswer, CheckedSealedError, SealedAnswerContext,
    SealedAnswerFrame, check_sealed_answer,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A grant that may answer one request on the sealed route.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SealedHolder {
    pub id: String,
    pub binding_id: u64,
    pub durable_session_id: String,
    pub connection_id: String,
    pub principal_id: String,
}

#[derive(Debug, Clone)]
pub struct SealedSecretOffer {
    pub ui_request_id: String,
    pub title: String,
    pub placeholder: Option<String>,
    pub deadline_at_ms: i64,
    pub hpke_pk: String,
}

/// Single-use `jti`s of one host incarnation (`PresenterJtiCache` in `mcp-presenter-attestation.ts`).
pub struct PresenterJtiCache {
    seen: HashMap<String, i64>,
    cap: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JtiAdmitResult {
    Ok,
    AttestationReplayed,
    PresenterBusy,
}

impl Default for PresenterJtiCache {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl PresenterJtiCache {
    pub fn new(cap: usize) -> Self {
        Self {
            seen: HashMap::new(),
            cap,
        }
    }

    pub fn admit(&mut self, jti: &str, exp: i64, now_s: i64) -> JtiAdmitResult {
        self.seen.retain(|_, until| *until > now_s);
        if self.seen.contains_key(jti) {
            return JtiAdmitResult::AttestationReplayed;
        }
        if self.seen.len() >= self.cap {
            return JtiAdmitResult::PresenterBusy;
        }
        self.seen.insert(jti.to_owned(), exp.saturating_add(60));
        JtiAdmitResult::Ok
    }

    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

/// What the secret route reads from the presenter control (D11 secret phase 5.1, 5.2).
pub trait SealedSecretSource: Send + Sync {
    fn holders(&self, session_id: &str) -> Vec<SealedHolder>;
    fn current(&self, holder: &SealedHolder) -> bool;
    fn connection_of(&self, holder: &SealedHolder) -> String;
    fn offer(&self, session_id: &str, holders: &[SealedHolder], offer: SealedSecretOffer);
    fn withdraw(&self, ui_request_id: &str);
    fn retire(&self, ui_request_id: &str);
    fn on_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    fn eligible(&self, holder: &SealedHolder) -> bool;
    fn principal_of(&self, holder: &SealedHolder) -> Option<String>;
    fn answer<'a>(
        &'a self,
        holder: &'a SealedHolder,
        frame: &'a SealedAnswerFrame,
    ) -> BoxFuture<'a, Result<SealedAnswer, SealedAnswerReply>>;
    fn settle(
        &self,
        holders: &[SealedHolder],
        ui_request_id: &str,
        outcome: SecretInputOutcome,
        winner_grant: Option<&str>,
    );
    fn admit_jti(&self, jti: &str, exp: i64) -> bool;
}

/// Host surface adapter that supplies session bindings, secret candidates, and frame sending.
pub trait SealedHoldersHost: Send + Sync {
    fn host_instance(&self) -> &str;
    fn binding_id_for_session(&self, session_id: &str) -> Option<u64>;
    fn secret_candidates(&self, binding_id: u64) -> Vec<SealedHolder>;
    fn is_current(&self, holder: &SealedHolder) -> bool;
    fn send_frame(&self, holder: &SealedHolder, frame: &McpPresenterSecretFrame);
    fn principal(&self, principal_id: &str) -> Option<AccountPrincipal>;
    fn subscribe_binding(
        &self,
        binding_id: u64,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn FnOnce() + Send>>;
}

struct Offered {
    durable_session_id: String,
    holders: HashSet<SealedHolder>,
    req: String,
    deadline_at_ms: i64,
    settled: bool,
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn account_lapsed(principal: &AccountPrincipal, account: &dyn SecretHostAuthority) -> bool {
    account.host_account().as_deref() != Some(principal.acc.as_str())
        || now_s() >= principal.attested_until
}

type HolderListenerEntry = (u64, Arc<dyn Fn() + Send + Sync>);

pub struct SealedSecretHolders {
    pub tokens: SecretHostTokens,
    host: Arc<dyn SealedHoldersHost>,
    account: Arc<dyn SecretHostAuthority>,
    offered: Mutex<HashMap<String, Offered>>,
    listeners: Arc<Mutex<Vec<HolderListenerEntry>>>,
    next_listener_id: Mutex<u64>,
    jtis: Mutex<PresenterJtiCache>,
    watched: Mutex<HashSet<u64>>,
    _token_unsub: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl SealedSecretHolders {
    pub fn create(
        host: Arc<dyn SealedHoldersHost>,
        account: Option<Arc<dyn SecretHostAuthority>>,
    ) -> Option<Arc<Self>> {
        let account_arc = account?;
        let tokens = SecretHostTokens::create(host.host_instance(), Some(account_arc.clone()))?;
        let offered = Mutex::new(HashMap::new());
        let holders = Arc::new(Self {
            tokens: tokens.clone(),
            host: host.clone(),
            account: account_arc,
            offered,
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: Mutex::new(1),
            jtis: Mutex::new(PresenterJtiCache::default()),
            watched: Mutex::new(HashSet::new()),
            _token_unsub: Mutex::new(None),
        });
        let weak = Arc::downgrade(&holders);
        let unsub = tokens.on_token(Arc::new(move |durable_session_id, token| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let to_send: Vec<SealedHolder> = {
                let offered = this
                    .offered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                offered
                    .values()
                    .filter(|o| o.durable_session_id == durable_session_id && !o.settled)
                    .flat_map(|o| o.holders.iter().cloned())
                    .filter(|h| this.host.is_current(h))
                    .collect()
            };
            let frame = McpPresenterSecretFrame::Token(McpPresenterSecretToken {
                v: 1,
                host_instance: this.host.host_instance().to_owned(),
                durable_session_id: durable_session_id.to_owned(),
                host_token: token.token.clone(),
            });
            for holder in to_send {
                this.host.send_frame(&holder, &frame);
            }
        }));
        *holders
            ._token_unsub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(unsub);
        Some(holders)
    }

    fn unlapsed(&self, holder: &SealedHolder) -> Option<AccountPrincipal> {
        let principal = self.host.principal(&holder.principal_id)?;
        (!account_lapsed(&principal, self.account.as_ref())).then_some(principal)
    }

    pub fn dispose(&self) {
        if let Some(unsub) = self
            ._token_unsub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            unsub();
        }
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.offered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.jtis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.tokens.dispose();
    }
}

impl SealedSecretSource for SealedSecretHolders {
    fn holders(&self, session_id: &str) -> Vec<SealedHolder> {
        let Some(binding_id) = self.host.binding_id_for_session(session_id) else {
            return Vec::new();
        };
        let need_watch = {
            let mut watched = self
                .watched
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            watched.insert(binding_id)
        };
        if need_watch {
            let listeners = self.listeners.clone();
            let _ = self.host.subscribe_binding(
                binding_id,
                Arc::new(move || {
                    let snapshot: Vec<_> = listeners
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .map(|(_, cb)| cb.clone())
                        .collect();
                    for cb in snapshot {
                        cb();
                    }
                }),
            );
        }
        self.host
            .secret_candidates(binding_id)
            .into_iter()
            .filter(|grant| self.eligible(grant))
            .collect()
    }

    fn eligible(&self, holder: &SealedHolder) -> bool {
        self.host
            .secret_candidates(holder.binding_id)
            .contains(holder)
            && self.unlapsed(holder).is_some()
            && self.tokens.current(&holder.durable_session_id).is_some()
    }

    fn principal_of(&self, holder: &SealedHolder) -> Option<String> {
        self.unlapsed(holder).map(|p| p.id)
    }

    fn answer<'a>(
        &'a self,
        holder: &'a SealedHolder,
        frame: &'a SealedAnswerFrame,
    ) -> BoxFuture<'a, Result<SealedAnswer, SealedAnswerReply>> {
        Box::pin(async move {
            let (durable_session_id, req, deadline_at_ms) = {
                let offered = self
                    .offered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(entry) = offered.get(&frame.ui_request_id) else {
                    return Err(SealedAnswerReply::Error(
                        SealedAnswerError::SecretAnswerRefused,
                    ));
                };
                if !entry.holders.contains(holder) {
                    return Err(SealedAnswerReply::Error(
                        SealedAnswerError::SecretAnswerRefused,
                    ));
                }
                (
                    entry.durable_session_id.clone(),
                    entry.req.clone(),
                    entry.deadline_at_ms,
                )
            };
            let Some(principal) = self.unlapsed(holder) else {
                return Err(SealedAnswerReply::Error(
                    SealedAnswerError::SecretAnswerRefused,
                ));
            };
            let host_token = self.tokens.current(&durable_session_id);
            let checked = check_sealed_answer(&SealedAnswerContext {
                frame,
                principal: &principal,
                req: &req,
                deadline_at_ms,
                host_token: host_token.as_ref(),
                account: self.account.as_ref(),
            })
            .await;
            match checked {
                CheckedSealedAnswer::Err(CheckedSealedError::InvalidSecretResponse) => Err(
                    SealedAnswerReply::Error(SealedAnswerError::InvalidSecretResponse),
                ),
                CheckedSealedAnswer::Err(CheckedSealedError::SecretStepupRefused) => Err(
                    SealedAnswerReply::Error(SealedAnswerError::SecretStepupRefused),
                ),
                CheckedSealedAnswer::Ok { jti, exp } => {
                    let value = match (&frame.value, jti, exp) {
                        (Some(v), Some(jti), Some(exp)) => Some(SealedAnswerValue {
                            jti,
                            exp,
                            enc: URL_SAFE_NO_PAD.encode(&v.enc),
                            ct: URL_SAFE_NO_PAD.encode(&v.ct),
                        }),
                        _ => None,
                    };
                    Ok(SealedAnswer {
                        ui_request_id: frame.ui_request_id.clone(),
                        principal: principal.id,
                        grant_id: frame.grant_id.clone(),
                        answer_id: frame.answer_id.clone(),
                        req,
                        value,
                    })
                }
            }
        })
    }

    fn admit_jti(&self, jti: &str, exp: i64) -> bool {
        self.jtis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admit(jti, exp, now_s())
            == JtiAdmitResult::Ok
    }

    fn settle(
        &self,
        holders: &[SealedHolder],
        ui_request_id: &str,
        outcome: SecretInputOutcome,
        winner_grant: Option<&str>,
    ) {
        let allowed: HashSet<SealedHolder> = {
            let offered = self
                .offered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(entry) = offered.get(ui_request_id) else {
                return;
            };
            entry.holders.clone()
        };
        for holder in holders {
            if !allowed.contains(holder) {
                continue;
            }
            let frame = McpPresenterSecretFrame::Settled(McpPresenterSecretSettled {
                v: 1,
                ui_request_id: ui_request_id.to_owned(),
                outcome,
                elsewhere: winner_grant != Some(holder.id.as_str()),
            });
            self.host.send_frame(holder, &frame);
        }
    }

    fn current(&self, holder: &SealedHolder) -> bool {
        self.host.is_current(holder)
    }

    fn connection_of(&self, holder: &SealedHolder) -> String {
        holder.connection_id.clone()
    }

    fn offer(&self, session_id: &str, holders: &[SealedHolder], offer: SealedSecretOffer) {
        let Some(first) = holders.first() else {
            return;
        };
        let durable_session_id = first.durable_session_id.clone();
        let Some(token) = self.tokens.current(&durable_session_id) else {
            return;
        };
        let Ok(binding) = secret_binding(&SecretBindingInput {
            host_instance: self.host.host_instance().to_owned(),
            host_jkt: self.tokens.host_jkt(),
            durable_session_id: durable_session_id.clone(),
            ui_request_id: offer.ui_request_id.clone(),
            deadline_at_ms: offer.deadline_at_ms,
            hpke_pk: offer.hpke_pk.clone(),
            title: offer.title.clone(),
            placeholder: offer.placeholder.clone(),
        }) else {
            return;
        };
        let Ok(sig) = self.tokens.sign_request(&binding) else {
            return;
        };
        let req = secret_request_hash(&binding);
        self.offered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                offer.ui_request_id.clone(),
                Offered {
                    durable_session_id: durable_session_id.clone(),
                    holders: holders.iter().cloned().collect(),
                    req,
                    deadline_at_ms: offer.deadline_at_ms,
                    settled: false,
                },
            );
        let frame = McpPresenterSecretFrame::Request(McpPresenterSecretRequest {
            v: 1,
            session_id: session_id.to_owned(),
            durable_session_id,
            ui_request_id: offer.ui_request_id,
            title: offer.title,
            placeholder: offer.placeholder,
            deadline_at_ms: offer.deadline_at_ms,
            host_instance: self.host.host_instance().to_owned(),
            hpke_pk: offer.hpke_pk,
            host_pk: self.tokens.host_pk(),
            host_token: token.token,
            sig,
        });
        for holder in holders {
            self.host.send_frame(holder, &frame);
        }
    }

    fn withdraw(&self, ui_request_id: &str) {
        self.offered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(ui_request_id);
    }

    fn retire(&self, ui_request_id: &str) {
        if let Some(entry) = self
            .offered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(ui_request_id)
        {
            entry.settled = true;
        }
    }

    fn on_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send> {
        let id = {
            let mut next = self
                .next_listener_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = *next;
            *next += 1;
            id
        };
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        let listeners = self.listeners.clone();
        Box::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(entry_id, _)| *entry_id != id);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jti_cache_enforces_single_use_expiry_and_no_eviction_on_full() {
        let mut cache = PresenterJtiCache::new(2);
        assert_eq!(cache.admit("j1", 100, 100), JtiAdmitResult::Ok);
        assert_eq!(
            cache.admit("j1", 100, 100),
            JtiAdmitResult::AttestationReplayed
        );
        assert_eq!(cache.admit("j2", 200, 100), JtiAdmitResult::Ok);
        // Full at cap 2: refuses rather than evicting j1.
        assert_eq!(cache.admit("j3", 300, 100), JtiAdmitResult::PresenterBusy);
        // After j1 expires (exp + 60 = 160), j3 is admitted.
        assert_eq!(cache.admit("j3", 300, 160), JtiAdmitResult::Ok);
    }
}
