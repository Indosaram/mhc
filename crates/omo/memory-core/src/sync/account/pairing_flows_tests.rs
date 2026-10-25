use super::{
    approve::{ApprovalConfirmation, ApproveOutcome, approve_device},
    http::create_account_http,
    join::{CeremonyClock, join_account},
    pairing::{PairingParty, approval_commitment},
    pairing_http::create_pairing_http,
};
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, creator_anchor},
        rejection::RejectionCounter,
        trust_pin::pin_trust_anchor,
    },
    engine::{adapters::http_transport::*, error::EngineError, ports::PortFuture},
    recovery::testing::{ACCOUNT, Server, device},
};
use serde_json::{Value, json};
use std::{cell::Cell, rc::Rc};
struct Clock;
impl CeremonyClock for Clock {
    fn now(&self) -> u64 {
        100
    }
    fn sleep(&self, _: u64) -> PortFuture<'_, ()> {
        panic!("fixture delivers exact state without polling")
    }
}
struct Service {
    rows: Vec<Value>,
    pending: Value,
    writes: Cell<usize>,
}
impl HttpClient for Service {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let value = if request.url.contains("/pending") {
                self.pending.clone()
            } else if request.url.contains("/keys") && request.method == "PUT" {
                self.writes.set(self.writes.get() + 1);
                json!({"seq":2})
            } else if request.url.contains("/keys") {
                json!({"entries":self.rows})
            } else {
                let after = request
                    .url
                    .rsplit("after=")
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                json!({"entries":self.rows.iter().filter(|r|r["seq"].as_u64().is_some_and(|s|s>after)).collect::<Vec<_>>()})
            };
            Ok(HttpResponse {
                status: 200,
                body: value.to_string().into_bytes(),
            })
        })
    }
    fn open_stream(&self, _: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(EngineError::Network) })
    }
}
fn options(client: Rc<dyn HttpClient>) -> HttpTransportOptions {
    HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
        client,
    }
}
#[tokio::test]
async fn approval_requires_matching_commitment_and_human_confirmation() {
    let dir = tempfile::tempdir().expect("temp");
    let member = Rc::new(device(1));
    let joining = device(2);
    let principal = super::join::principal_of(&joining);
    let server = Server::new(member.as_ref(), dir.path());
    let rows = server.rows.lock().expect("rows").clone();
    let commitment = approval_commitment(
        ACCOUNT,
        &PairingParty {
            principal: &principal,
            nonce: "nonce",
        },
    )
    .expect("commitment");
    let pending = json!({"id":"pending","account":ACCOUNT,"commitment":commitment,"answer":null,"reveal":{"principal":principal,"joining_nonce":"nonce"},"expires_at":1000});
    let service = Rc::new(Service {
        rows: rows.clone(),
        pending,
        writes: Cell::new(0),
    });
    let verified = super::account_chain::open_account_chain(
        ACCOUNT.into(),
        ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), member.as_ref())),
        member.clone(),
        create_account_http(options(service.clone())),
        RejectionCounter::default(),
    )
    .await
    .expect("chain");
    pin_trust_anchor(dir.path(), ACCOUNT, verified.state().pin().expect("pin")).expect("pin save");
    let pairing = create_pairing_http(create_account_http(options(service.clone())));
    let outcome = approve_device(
        ACCOUNT,
        member.clone(),
        dir.path(),
        create_account_http(options(service.clone())),
        &pairing,
        &Clock,
        ApprovalConfirmation { pending_id: "pending", confirm_code: &|_| Box::pin(async { Ok(false) }) },
    )
    .await
    .expect("decline");
    assert_eq!(outcome, ApproveOutcome::Declined);
    assert_eq!(service.writes.get(), 0);
    let outcome = approve_device(
        ACCOUNT,
        member.clone(),
        dir.path(),
        create_account_http(options(service.clone())),
        &pairing,
        &Clock,
        ApprovalConfirmation { pending_id: "pending", confirm_code: &|code| {
            Box::pin(async move {
                assert_eq!(code.len(), 6);
                Ok(true)
            })
        } },
    )
    .await
    .expect("approve");
    assert_eq!(outcome, ApproveOutcome::Approved(2));
    assert_eq!(service.writes.get(), 1);
    let mut pending = service.pending.clone();
    pending["commitment"] = json!("substituted");
    let bad = Rc::new(Service {
        rows,
        pending,
        writes: Cell::new(0),
    });
    let pairing = create_pairing_http(create_account_http(options(bad.clone())));
    let outcome = approve_device(
        ACCOUNT,
        member,
        dir.path(),
        create_account_http(options(bad.clone())),
        &pairing,
        &Clock,
        ApprovalConfirmation { pending_id: "pending", confirm_code: &|_| panic!("must not confirm substituted keys") },
    )
    .await
    .expect("refusal");
    assert!(matches!(outcome, ApproveOutcome::Refused(_)));
    assert_eq!(bad.writes.get(), 0);
}
#[tokio::test]
async fn joining_expires_without_reveal_or_pin() {
    let dir = tempfile::tempdir().expect("temp");
    let pending = json!({"id":"p","account":ACCOUNT,"commitment":"c","answer":null,"reveal":null,"expires_at":1});
    let service = Rc::new(Service {
        rows: vec![],
        pending,
        writes: Cell::new(0),
    });
    let pairing = create_pairing_http(create_account_http(options(service.clone())));
    let outcome = join_account(
        ACCOUNT,
        Rc::new(device(1)),
        dir.path(),
        create_account_http(options(service)),
        &pairing,
        &Clock,
        &|_| panic!("expired pairing has no code"),
    )
    .await
    .expect("expired");
    assert_eq!(outcome, super::join::JoinOutcome::Expired);
    assert!(!dir.path().join("trust-anchor").exists());
}
