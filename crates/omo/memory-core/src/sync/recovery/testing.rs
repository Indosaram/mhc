use super::{derive::*, key_material::*, keyring_append::signed_append, ports::*};
use crate::sync::{
    crypto::{
        entry::{Head, SyncAuthor},
        links::wrap_set_digest,
        parse::{Principal, RecoveryKey, Survivable},
        signatures::*,
    },
    keyring_port::*,
};
use aws_lc_rs::signature::Ed25519KeyPair;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::sync::Mutex;
use zeroize::Zeroizing;

pub const ACCOUNT: &str = "recovery-test";
pub const RP: &str = "recovery.example";
pub fn device(seed: u8) -> DeviceKeyringImpl {
    let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("fixture seed");
    let signing = URL_SAFE_NO_PAD.encode(pair.to_pkcs8v1().expect("fixture PKCS8").as_ref());
    let mut sealing = vec![
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04,
        0x20,
    ];
    sealing.extend([seed; 32]);
    let material = DeviceKeyMaterial {
        signing_private_pkcs8: signing,
        sealing_private_pkcs8: URL_SAFE_NO_PAD.encode(sealing),
    };
    let temporary = create_device_keyring(String::new(), &material).expect("fixture keyring");
    create_device_keyring(
        device_id_of(&temporary.public_keys().signing).expect("device id"),
        &material,
    )
    .expect("fixture keyring")
}
pub fn principal(keyring: &dyn DeviceKeyring) -> Principal {
    let keys = keyring.public_keys();
    Principal {
        kind: "device".into(),
        id: keyring.principal().id,
        sealing_public_key: keys.sealing,
        signing_public_key: keys.signing,
    }
}
pub struct Authenticator {
    pub credential: String,
    pub supported: bool,
}
impl Authenticator {
    pub fn enrolled() -> Self {
        Self {
            credential: "first".into(),
            supported: true,
        }
    }
}
impl PasskeyAuthenticator for Authenticator {
    fn prf_supported(&self) -> bool {
        self.supported
    }
    fn create(&self, _request: CreateRequest) -> PortFuture<'_, Option<PasskeyCreation>> {
        Box::pin(async move {
            Ok(Some(PasskeyCreation {
                credential_id: self.credential.clone(),
                attachment: Attachment::Platform,
                flags: PasskeyFlags {
                    user_verified: true,
                    backup_eligible: true,
                    backup_state: true,
                },
                prf_output: Some(Zeroizing::new(vec![7; 32])),
            }))
        })
    }
    fn assert(&self, request: AssertRequest) -> PortFuture<'_, Option<PasskeyAssertion>> {
        Box::pin(async move {
            Ok(request
                .prf_salts
                .contains_key(&self.credential)
                .then(|| PasskeyAssertion {
                    credential_id: self.credential.clone(),
                    user_handle: recovery_user_handle(ACCOUNT),
                    flags: PasskeyFlags {
                        user_verified: true,
                        backup_eligible: true,
                        backup_state: true,
                    },
                    prf_output: Some(Zeroizing::new(vec![7; 32])),
                }))
        })
    }
    fn discover<'a>(&'a self, _rp: &'a str) -> PortFuture<'a, Option<Vec<DiscoveredCredential>>> {
        Box::pin(async { Ok(None) })
    }
    fn signal_removed<'a>(&'a self, _rp: &'a str, _id: &'a str) -> PortFuture<'a, PlatformSignal> {
        Box::pin(async { Ok(PlatformSignal::Unsupported) })
    }
}
pub struct Server {
    pub rows: Mutex<Vec<Value>>,
    pub author: SyncAuthor,
    pub refuse_rotation: bool,
    pub acknowledge_rotation_only: bool,
    pub state_dir: std::path::PathBuf,
    pub assert_hold_before_join: bool,
    pub acknowledge_join_only: bool,
}
impl Server {
    pub fn new(member: &dyn DeviceKeyring, state_dir: &std::path::Path) -> Self {
        let p = principal(member);
        let author = SyncAuthor::Device {
            id: p.id.clone(),
            sealing_public_key: p.sealing_public_key.clone(),
            signing_public_key: p.signing_public_key.clone(),
        };
        let keys = derive_recovery_keys(&[7; 32], &[8; 32]).expect("fixture derivation");
        let (sealing_public_key, signing_public_key) = public_key_text(&keys);
        let recovery = RecoveryKey {
            credential_id: "first".into(),
            rp_id: RP.into(),
            enrollment_nonce: URL_SAFE_NO_PAD.encode([8; 32]),
            sealing_public_key,
            signing_public_key,
            survivable: Survivable::Synced,
        };
        let material = EpochMaterial {
            epoch: 1,
            current: Zeroizing::new(vec![9; 32]),
            history: Vec::new(),
        };
        let wraps = vec![
            seal_wrap(
                &WrapAddress {
                    account: ACCOUNT,
                    epoch: 1,
                    id: &format!("device:{}", p.id),
                },
                &p.sealing_public_key,
                &material,
            )
            .expect("device wrap"),
            seal_wrap(
                &WrapAddress {
                    account: ACCOUNT,
                    epoch: 1,
                    id: "recovery:first",
                },
                &recovery.sealing_public_key,
                &material,
            )
            .expect("recovery wrap"),
        ];
        let bootstrap = json!({"account":ACCOUNT,"custody_nonce":URL_SAFE_NO_PAD.encode([3;32]),"first_device":p,"recovery_key":recovery});
        let root = tagged_hash("omo-sync/v1/custody-root", &bootstrap).expect("root hash");
        let digest = wrap_set_digest(&wraps).expect("wrap digest");
        let countersign = sign_framed("omo-sync/v1/recovery-countersign",&json!({"t":"omo-sync/v1/recovery-countersign","account":ACCOUNT,"custody_root":root,"predecessor":"","wraps_digest":digest}),keys.sign_seed.as_ref()).expect("countersign");
        let body = json!({"kind":"genesis","account":ACCOUNT,"epoch":1,"bootstrap":bootstrap,"custody_root":root,"countersign":countersign,"wraps_digest":digest});
        let request = signed_append(
            member,
            &Head {
                seq: 0,
                link: String::new(),
                keyring_link: String::new(),
            },
            body,
            wraps,
        )
        .expect("genesis append");
        let server = Self {
            rows: Mutex::new(Vec::new()),
            author,
            refuse_rotation: false,
            acknowledge_rotation_only: false,
            state_dir: state_dir.into(),
            assert_hold_before_join: false,
            acknowledge_join_only: false,
        };
        server.store(request, false);
        server
    }
    fn store(&self, request: KeyringAppendRequest, recovery: bool) -> u64 {
        let mut rows = self.rows.lock().expect("fixture lock");
        let seq = u64::try_from(rows.len()).expect("fixture length") + 1;
        let digest = sha256_b64(&canonical(&request.record).expect("record canonical"));
        let author = if recovery {
            SyncAuthor::Recovery {
                credential_id: request.record["credential_id"]
                    .as_str()
                    .expect("credential")
                    .into(),
            }
        } else {
            self.author.clone()
        };
        rows.push(json!({"seq":seq,"base_seq":request.base_seq,"kind":"keyring","account":ACCOUNT,"epoch":request.record["epoch"],"author":author,"digest":digest,"link":request.link,"keyring_link":request.keyring_link,"record_sig":request.record_sig,"body":request.record,"wraps":request.wraps}));
        seq
    }
}
impl RecoveryTransport for Server {
    fn list_keys(&self) -> PortFuture<'_, Value> {
        Box::pin(async { Ok(json!(self.rows.lock().expect("fixture lock").clone())) })
    }
    fn list_entries(&self) -> PortFuture<'_, Value> {
        Box::pin(async {
            Ok(Value::Array(
                self.rows
                    .lock()
                    .expect("fixture lock")
                    .iter()
                    .map(|row| {
                        let mut row = row.clone();
                        row.as_object_mut().expect("row").remove("body");
                        row.as_object_mut().expect("row").remove("wraps");
                        row
                    })
                    .collect(),
            ))
        })
    }
    fn list_recovery_wraps(&self) -> PortFuture<'_, Value> {
        Box::pin(async {
            Ok(Value::Array(self.rows.lock().expect("fixture lock").iter().flat_map(|row| row["wraps"].as_array().expect("wraps").iter().map(|wrap| json!({"record":{"seq":row["seq"],"digest":row["digest"],"keyring_link":row["keyring_link"]},"entry":wrap}))).collect()))
        })
    }
    fn append_keys(&self, request: KeyringAppendRequest) -> PortFuture<'_, AppendResult> {
        Box::pin(async move {
            if request.record["kind"] == "rotation" {
                if self.refuse_rotation {
                    return Ok(Err(TransportRefusal::Refused));
                }
                if self.acknowledge_rotation_only {
                    return Ok(Ok(request.base_seq + 1));
                }
            }
            Ok(Ok(self.store(request, false)))
        })
    }
    fn recovery_join(&self, request: KeyringAppendRequest) -> PortFuture<'_, AppendResult> {
        Box::pin(async move {
            if self.assert_hold_before_join {
                assert!(matches!(
                    super::write_hold::load_write_hold(&self.state_dir, ACCOUNT)?,
                    super::write_hold::WriteHold::Held { .. }
                ));
            }
            if self.acknowledge_join_only {
                return Ok(Ok(request.base_seq + 1));
            }
            Ok(Ok(self.store(request, true)))
        })
    }
}
