use super::{account_chain::*, http::*, session::*};
use crate::sync::keyring_port::DeviceKeyring;
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, creator_anchor},
        rejection::RejectionCounter,
    },
    engine::{adapters::http_transport::*, error::EngineError, ports::PortFuture},
    recovery::testing::{ACCOUNT, Server, device},
};
use serde_json::{Value, json};
use std::{cell::Cell, rc::Rc};
struct Service {
    rows: std::cell::RefCell<Vec<Value>>,
    mismatch: Cell<bool>,
}
impl HttpClient for Service {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let body = if request.url.contains("/keys") {
                let mut rows = self.rows.borrow().clone();
                if self.mismatch.get() {
                    rows[0]["digest"] = json!("mismatch");
                }
                json!({"entries":rows})
            } else {
                let after = request
                    .url
                    .rsplit("after=")
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                json!({"entries":self.rows.borrow().iter().filter(|r|r["seq"].as_u64().is_some_and(|seq|seq>after)).cloned().collect::<Vec<_>>()})
            };
            Ok(HttpResponse {
                status: 200,
                body: body.to_string().into_bytes(),
            })
        })
    }
    fn open_stream(&self, _request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(EngineError::Other("unused stream".into())) })
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
async fn verified_wrap_keys_and_pending_retry() {
    let dir = tempfile::tempdir().expect("temp");
    let keyring = Rc::new(device(1));
    let server = Server::new(keyring.as_ref(), dir.path());
    let service = Rc::new(Service {
        rows: std::cell::RefCell::new(server.rows.lock().expect("rows").clone()),
        mismatch: Cell::new(true),
    });
    let mut chain = open_account_chain(
        ACCOUNT.into(),
        ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), keyring.as_ref())),
        keyring,
        create_account_http(options(service.clone())),
        RejectionCounter::default(),
    )
    .await
    .expect("chain");
    assert!(matches!(
        chain.reach(1).await.expect("position"),
        ChainPosition::Pending
    ));
    assert!(!chain.is_member());
    assert!(chain.epoch_key(1).is_none());
    service.mismatch.set(false);
    assert!(matches!(
        chain.reach(1).await.expect("retry"),
        ChainPosition::Keyring
    ));
    assert!(chain.is_member());
    assert_eq!(chain.epoch_key(1), Some(vec![9; 32]));
    assert!(chain.link_at(1).is_some());
    assert!(chain.link_at(2).is_none());
}
#[tokio::test]
async fn no_pin_idles_without_http() {
    struct NoNetwork;
    impl HttpClient for NoNetwork {
        fn send(&self, _: HttpRequest) -> PortFuture<'_, HttpResponse> {
            panic!("no pin must not send requests")
        }
        fn open_stream(&self, _: HttpRequest) -> PortFuture<'_, StreamResponse> {
            panic!("no pin must not stream")
        }
    }
    let dir = tempfile::tempdir().expect("temp");
    let result = open_account_session(AccountSessionOptions {
        http: options(Rc::new(NoNetwork)),
        account: ACCOUNT.into(),
        keyring: Rc::new(device(1)),
        pin_dir: dir.path().into(),
        space_id: vec![1; 16],
        peer: None,
        now: None,
    })
    .await
    .expect("idle");
    assert!(matches!(
        result,
        AccountSessionOutcome::Idle(AccountSessionIdle::NoPin)
    ));
}

#[tokio::test]
async fn chain_seals_and_verifies_own_space_without_applying_other_spaces() {
    use super::{join::principal_of, sealer::create_chain_sealer, verifier::create_chain_verifier};
    use crate::sync::{
        crypto::signatures::sha256_b64,
        engine::ports::{
            BatchPayload, EntryKind, PayloadItem, PulledEntry, Sealer, Verdict, Verifier,
        },
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let dir = tempfile::tempdir().expect("temp");
    let keyring = Rc::new(device(1));
    let server = Server::new(keyring.as_ref(), dir.path());
    let service = Rc::new(Service {
        rows: std::cell::RefCell::new(server.rows.lock().expect("rows").clone()),
        mismatch: Cell::new(false),
    });
    let chain = Rc::new(tokio::sync::Mutex::new(
        open_account_chain(
            ACCOUNT.into(),
            ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), keyring.as_ref())),
            keyring.clone(),
            create_account_http(options(service.clone())),
            RejectionCounter::default(),
        )
        .await
        .expect("chain"),
    ));
    let sealer = create_chain_sealer(
        chain.clone(),
        keyring.clone(),
        vec![1; 16],
        None,
        Rc::new(|| 123),
    );
    let batch_id = URL_SAFE_NO_PAD.encode([3; 16]);
    let payload = BatchPayload {
        items: vec![PayloadItem {
            key: "a.md".into(),
            ops: vec![1, 2, 3],
        }],
    };
    let sealed = sealer.seal(&batch_id, 1, &payload).await.expect("seal");
    let digest = sha256_b64(&sealed.bytes);
    service.rows.borrow_mut().push(json!({"seq":2,"base_seq":1,"kind":"batch","account":ACCOUNT,"epoch":sealed.epoch,"author":principal_of(keyring.as_ref()),"digest":digest,"link":sealed.link,"keyring_link":null,"record_sig":sealed.record_sig}));
    let entry = PulledEntry {
        seq: 2,
        kind: EntryKind::Batch,
        batch_id: Some(batch_id.clone()),
        digest: hex_of_b64url(&digest),
        object_id: "obj".into(),
        author: format!("device:{}", keyring.principal().id),
    };
    let verifier = create_chain_verifier(chain.clone(), vec![1; 16], RejectionCounter::default());
    assert_eq!(
        verifier
            .verify_entry(&entry, &sealed.bytes)
            .await
            .expect("verify"),
        Verdict::Ok {
            payload,
            batch_id: batch_id.clone()
        }
    );
    let other = create_chain_verifier(chain, vec![2; 16], RejectionCounter::default());
    assert_eq!(
        other
            .verify_entry(&entry, &sealed.bytes)
            .await
            .expect("other space"),
        Verdict::Ok {
            payload: BatchPayload::default(),
            batch_id
        }
    );
    assert!(
        sealer
            .seal(
                &URL_SAFE_NO_PAD.encode([4; 16]),
                99,
                &BatchPayload::default()
            )
            .await
            .is_err()
    );
}
