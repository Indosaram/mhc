//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-secret-route.ts`.
//!
//! A worker isolate's secret-prompt route (senpi#5192; D11 secret phase 5.5). The host selects
//! the presenter and authenticates sealed answers; this isolate keeps the latch, the per-request
//! key, the receipts and the step-up `jti` cache, and claims, opens and settles here.

use crate::secret_input_requests::{
    SealedAnswer, SealedAnswerError, SealedAnswerReply, SealedClaim, SealedLatch,
    SealedOfferDetails, SecretInputRoute, SecretInputUnavailableReason,
};
use crate::secret_sealed::SecretInputOutcome;
use crate::secret_sealed_holders::{JtiAdmitResult, PresenterJtiCache};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub type SecretRouteExchange = Arc<dyn Fn(WorkerSecretExchangeMsg) + Send + Sync>;

pub enum WorkerSecretExchangeMsg {
    Select {
        id: String,
        connection: Option<String>,
        decision: Arc<Mutex<Option<Value>>>,
    },
    Frame {
        id: String,
        record: Value,
    },
    Settled {
        id: String,
        outcome: Option<SecretInputOutcome>,
        winner_grant: Option<String>,
    },
    Forget {
        id: String,
    },
    Handshake {
        capabilities: Vec<String>,
    },
}

struct PresenterEntry {
    connection: Option<String>,
    raiser: bool,
    sealed: bool,
}

struct OutcomeEntry {
    outcome: SecretInputOutcome,
    winner_grant: Option<String>,
}

struct IsolateClaim {
    jtis: Arc<Mutex<PresenterJtiCache>>,
}

impl SealedClaim for IsolateClaim {
    fn current(&self) -> bool {
        true
    }

    fn admit_jti(&self, jti: &str, exp: i64) -> bool {
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.jtis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admit(jti, exp, now_s)
            == JtiAdmitResult::Ok
    }
}

type CancelMap = Arc<Mutex<HashMap<String, Box<dyn FnOnce() + Send + Sync>>>>;

struct RouteInner {
    exchange: SecretRouteExchange,
    current_connection: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    cancels: CancelMap,
    latch: Arc<Mutex<Option<Arc<dyn SealedLatch>>>>,
    presenters: Mutex<HashMap<String, PresenterEntry>>,
    outcomes: Mutex<HashMap<String, OutcomeEntry>>,
}

pub struct WorkerSecretRoutes {
    cancels: CancelMap,
    latch: Arc<Mutex<Option<Arc<dyn SealedLatch>>>>,
    jtis: Arc<Mutex<PresenterJtiCache>>,
    exchange: SecretRouteExchange,
    current_connection: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl WorkerSecretRoutes {
    pub fn new(
        exchange: SecretRouteExchange,
        current_connection: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            cancels: Arc::new(Mutex::new(HashMap::new())),
            latch: Arc::new(Mutex::new(None)),
            jtis: Arc::new(Mutex::new(PresenterJtiCache::default())),
            exchange,
            current_connection,
        }
    }

    pub fn cancel_request(&self, id: &str) {
        if let Some(cancel) = self
            .cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id)
        {
            cancel();
        }
    }

    pub fn route(&self) -> Arc<dyn SecretInputRoute> {
        Arc::new(RouteInner {
            exchange: self.exchange.clone(),
            current_connection: self.current_connection.clone(),
            cancels: self.cancels.clone(),
            latch: self.latch.clone(),
            presenters: Mutex::new(HashMap::new()),
            outcomes: Mutex::new(HashMap::new()),
        })
    }

    pub async fn answer(&self, answer: SealedAnswer) -> SealedAnswerReply {
        let latch = self
            .latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(latch) = latch else {
            return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
        };
        if let Some(settled) =
            latch.settled(&answer.ui_request_id, &answer.answer_id, &answer.principal)
        {
            return settled;
        }
        let claim = IsolateClaim {
            jtis: self.jtis.clone(),
        };
        latch.respond(answer, &claim).await
    }
}

impl SecretInputRoute for RouteInner {
    fn select(
        &self,
        id: &str,
        unavailable: Box<dyn FnOnce() + Send + Sync>,
    ) -> Option<SecretInputUnavailableReason> {
        let decision = Arc::new(Mutex::new(None));
        (self.exchange)(WorkerSecretExchangeMsg::Select {
            id: id.to_owned(),
            connection: (self.current_connection)(),
            decision: decision.clone(),
        });
        let value = decision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        let accepted = value
            .get("accepted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !accepted {
            return value
                .get("reason")
                .and_then(Value::as_str)
                .and_then(SecretInputUnavailableReason::parse)
                .or(Some(SecretInputUnavailableReason::NoPresenter));
        }
        let connection = value
            .get("connection")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let raiser = value.get("raiser").and_then(Value::as_bool).unwrap_or(true);
        let sealed = value
            .get("sealed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.to_owned(),
                PresenterEntry {
                    connection,
                    raiser,
                    sealed,
                },
            );
        self.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), unavailable);
        None
    }

    fn accepts(&self, id: &str) -> bool {
        let current = (self.current_connection)();
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .is_some_and(|p| p.raiser && p.connection == current)
    }

    fn available(&self, id: &str) -> bool {
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(id)
    }

    fn presenter(&self, id: &str) -> Option<String> {
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .and_then(|p| p.connection.clone())
    }

    fn sealed(&self, id: &str) -> bool {
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .is_some_and(|p| p.sealed)
    }

    fn raiser(&self, id: &str) -> bool {
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .is_some_and(|p| p.raiser)
    }

    fn send(&self, id: &str, record: Value) {
        (self.exchange)(WorkerSecretExchangeMsg::Frame {
            id: id.to_owned(),
            record,
        });
    }

    fn offer_sealed(&self, id: &str, offer: SealedOfferDetails) {
        let mut record = json!({
            "type": "secret_sealed_offer",
            "hpkePk": offer.hpke_pk,
            "title": offer.title,
            "deadlineAtMs": offer.deadline_at_ms,
        });
        if let Some(p) = offer.placeholder {
            record["placeholder"] = json!(p);
        }
        (self.exchange)(WorkerSecretExchangeMsg::Frame {
            id: id.to_owned(),
            record,
        });
    }

    fn sealed_outcome(&self, id: &str, outcome: SecretInputOutcome, winner_grant: Option<&str>) {
        self.outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.to_owned(),
                OutcomeEntry {
                    outcome,
                    winner_grant: winner_grant.map(str::to_owned),
                },
            );
    }

    fn bind_sealed_responder(&self, latch: Option<Arc<dyn SealedLatch>>) {
        *self
            .latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = latch;
    }

    fn settled(&self, id: &str) {
        self.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        let entry = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        (self.exchange)(WorkerSecretExchangeMsg::Settled {
            id: id.to_owned(),
            outcome: entry.as_ref().map(|e| e.outcome),
            winner_grant: entry.and_then(|e| e.winner_grant),
        });
    }

    fn forget(&self, id: &str) {
        self.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        self.presenters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        self.outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        (self.exchange)(WorkerSecretExchangeMsg::Forget { id: id.to_owned() });
    }

    fn handshake(&self, capabilities: &[String]) {
        if (self.current_connection)().is_none() {
            (self.exchange)(WorkerSecretExchangeMsg::Handshake {
                capabilities: capabilities.to_vec(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tracks_presenter_raiser_and_sealed_flags() {
        let exchange: SecretRouteExchange = Arc::new(|msg| {
            if let WorkerSecretExchangeMsg::Select { decision, .. } = msg {
                *decision
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(json!({
                    "accepted": true,
                    "connection": "conn-1",
                    "raiser": false,
                    "sealed": true,
                }));
            }
        });
        let routes = WorkerSecretRoutes::new(exchange, Arc::new(|| Some("conn-1".into())));
        let route = routes.route();
        assert_eq!(route.select("r1", Box::new(|| {})), None);
        assert!(route.sealed("r1"));
        assert!(!route.raiser("r1"));
        // Sealed-only request never accepts forwarded raiser response.
        assert!(!route.accepts("r1"));
    }
}
