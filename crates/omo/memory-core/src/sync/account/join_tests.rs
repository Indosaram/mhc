use super::{
    http::create_account_http,
    join::{CeremonyClock, JoinOutcome, join_account, principal_of},
    pairing_http::create_pairing_http,
};
use crate::sync::{
    crypto::{
        entry::Head,
        links::wrap_set_digest,
        signatures::{canonical, sha256_b64},
        trust_pin::{PinLoad, load_trust_anchor},
    },
    engine::{adapters::http_transport::*, error::EngineError, ports::PortFuture},
    keyring_port::DeviceKeyringImpl,
    recovery::{
        key_material::{EpochMaterial, WrapAddress, seal_wrap},
        keyring_append::signed_append,
        testing::{ACCOUNT, Server, device},
    },
};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};
use zeroize::Zeroizing;
struct Clock;
impl CeremonyClock for Clock {
    fn now(&self) -> u64 {
        0
    }
    fn sleep(&self, _: u64) -> PortFuture<'_, ()> {
        panic!("all transitions are delivered by the triggering response")
    }
}
struct Service {
    member: DeviceKeyringImpl,
    rows: RefCell<Vec<Value>>,
    pending: RefCell<Value>,
}
impl HttpClient for Service {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let body = request
                .body
                .as_ref()
                .map(|b| serde_json::from_slice::<Value>(b).expect("request json"))
                .unwrap_or(Value::Null);
            let response = if request.url.ends_with("/pending") && request.method == "PUT" {
                let row = json!({"id":"p","account":ACCOUNT,"commitment":body["commitment"],"answer":{"principal":principal_of(&self.member),"approver_nonce":"approver-nonce"},"reveal":null,"expires_at":1000});
                *self.pending.borrow_mut() = row.clone();
                row
            } else if request.url.ends_with("/reveal") {
                let principal = body["principal"].clone();
                let nonce = body["joining_nonce"].clone();
                let rows = self.rows.borrow();
                let last = rows.last().expect("genesis");
                let head = Head {
                    seq: last["seq"].as_u64().expect("seq"),
                    link: last["link"].as_str().expect("link").into(),
                    keyring_link: last["keyring_link"].as_str().expect("keyring link").into(),
                };
                drop(rows);
                let material = EpochMaterial {
                    epoch: 1,
                    current: Zeroizing::new(vec![9; 32]),
                    history: vec![],
                };
                let wraps = vec![
                    seal_wrap(
                        &WrapAddress {
                            account: ACCOUNT,
                            epoch: 1,
                            id: &format!("device:{}", principal["id"].as_str().expect("principal")),
                        },
                        principal["sealing_public_key"].as_str().expect("key"),
                        &material,
                    )
                    .expect("wrap"),
                ];
                let record = json!({"kind":"approval","account":ACCOUNT,"epoch":1,"principal":principal,"wraps_digest":wrap_set_digest(&wraps).expect("digest"),"pairing":{"commitment":self.pending.borrow()["commitment"],"joining_nonce":nonce,"approver_nonce":"approver-nonce"}});
                let append =
                    signed_append(&self.member, &head, record, wraps).expect("signed approval");
                let digest = sha256_b64(&canonical(&append.record).expect("canonical"));
                self.rows.borrow_mut().push(json!({"seq":2,"base_seq":1,"kind":"keyring","account":ACCOUNT,"epoch":1,"author":principal_of(&self.member),"digest":digest,"link":append.link,"keyring_link":append.keyring_link,"record_sig":append.record_sig,"body":append.record,"wraps":append.wraps}));
                let mut pending = self.pending.borrow().clone();
                pending["reveal"] = json!({"principal":principal,"joining_nonce":nonce});
                pending
            } else if request.url.contains("/keys") {
                json!({"entries":self.rows.borrow().clone()})
            } else {
                let after = request
                    .url
                    .rsplit("after=")
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                json!({"entries":self.rows.borrow().iter().filter(|r|r["seq"].as_u64().is_some_and(|s|s>after)).cloned().collect::<Vec<_>>()})
            };
            Ok(HttpResponse {
                status: 200,
                body: response.to_string().into_bytes(),
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
async fn joining_pins_only_verified_ceremony_admission_and_openable_key() {
    let dir = tempfile::tempdir().expect("temp");
    let member = device(1);
    let server = Server::new(&member, dir.path());
    let service = Rc::new(Service {
        member,
        rows: RefCell::new(server.rows.lock().expect("rows").clone()),
        pending: RefCell::new(Value::Null),
    });
    let pairing = create_pairing_http(create_account_http(options(service.clone())));
    let outcome = join_account(
        ACCOUNT,
        Rc::new(device(2)),
        dir.path(),
        create_account_http(options(service)),
        &pairing,
        &Clock,
        &|code| {
            Box::pin(async move {
                assert_eq!(code.len(), 6);
                Ok(())
            })
        },
    )
    .await
    .expect("join");
    assert_eq!(outcome, JoinOutcome::Joined);
    assert!(matches!(
        load_trust_anchor(dir.path(), ACCOUNT).expect("pin"),
        PinLoad::Loaded { .. }
    ));
}
