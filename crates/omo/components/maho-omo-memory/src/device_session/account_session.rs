//! Native trust-pinned account session, using the memory-core crypto primitives.
use super::resolver::{AccountSessionIdle, AccountSessionOptions, LocalFuture};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use memory_core::sync::{
    crypto::{
        anchor::ChainAnchor,
        build::{BuildBatchInput, build_batch, sign_record},
        chain::{AppendOutcome, ChainState, ChainStep, append_entries, empty_chain},
        entry::SyncRecordFields,
        links::next_link,
        open::{BatchBinding, OpenOutcome, open_batch},
        rejection::RejectionCounter,
        trust_pin::{PinLoad, load_trust_anchor},
        vendor::sync_format::object_types::{ObjectKind, SyncObjectRecord},
    },
    engine::{
        SyncSession,
        adapters::http_transport::*,
        error::EngineError,
        ports::*,
    },
    keyring_port::{DeviceKeyring, create_device_keyring},
    recovery::{
        keyring_append::own_material,
        served_chain::{committed_wrap, expected_history},
    },
};
use serde_json::Value;
use std::{collections::BTreeMap, rc::Rc};

struct Client(reqwest::Client);
struct Stream(reqwest::Response);
impl ByteStream for Stream {
    fn next_chunk(&mut self) -> PortFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            self.0
                .chunk()
                .await
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .map_err(|error| EngineError::Other(error.to_string()))
        })
    }
}
impl HttpClient for Client {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let mut builder = self.0.request(
                reqwest::Method::from_bytes(request.method.as_bytes())
                    .map_err(|error| EngineError::Other(error.to_string()))?,
                request.url,
            );
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?
                .to_vec();
            Ok(HttpResponse { status, body })
        })
    }
    fn open_stream(&self, request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async move {
            let mut builder = self.0.get(request.url);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| EngineError::Other(error.to_string()))?;
            Ok(StreamResponse {
                status: response.status().as_u16(),
                body: Box::new(Stream(response)),
            })
        })
    }
}
struct Chain {
    state: ChainState,
    positions: BTreeMap<u64, ChainStep>,
    links: BTreeMap<u64, String>,
    refused: bool,
    account: String,
    api: String,
    token: Rc<dyn Fn() -> LocalFuture<String>>,
    client: reqwest::Client,
    keyring: Box<dyn DeviceKeyring>,
    space: [u8; 16],
}
impl Chain {
    async fn read(&self, path: &str) -> Result<Value, String> {
        let response = self
            .client
            .get(format!("{}{path}", self.api))
            .bearer_auth((self.token)().await?)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "account sync request failed ({})",
                response.status().as_u16()
            ));
        }
        response.json().await.map_err(|error| error.to_string())
    }
    async fn extend(&mut self, target: u64) -> Result<(), String> {
        while !self.refused {
            let after = self.state.head().map_or(0, |head| head.seq);
            let response = self
                .read(&format!("/v1/sync/entries?after={after}"))
                .await?;
            let rows = response["entries"]
                .as_array()
                .ok_or("account sync: entries read did not answer a list")?;
            if rows.is_empty() {
                break;
            }
            let key_response = if rows.iter().any(|row| row["kind"] == "keyring") {
                self.read("/v1/sync/keys").await?
            } else {
                serde_json::json!({"entries":[]})
            };
            let keys = key_response["entries"]
                .as_array()
                .ok_or("account sync: keyring read did not answer a list")?;
            let mut entries = Vec::new();
            for row in rows {
                if row["kind"] == "batch" {
                    let mut entry = row.clone();
                    entry["keyring_link"] = Value::Null;
                    entries.push(entry);
                } else if let Some(key) = keys.iter().find(|key| {
                    key["seq"] == row["seq"]
                        && key["digest"] == row["digest"]
                        && key["keyring_link"] == row["keyring_link"]
                }) {
                    entries.push(key.clone());
                } else {
                    break;
                }
            }
            if entries.is_empty() {
                break;
            }
            let (state, steps) =
                match append_entries(&self.state, &entries, &mut RejectionCounter::default()) {
                    AppendOutcome::Accepted { state, steps } => (state, steps),
                    AppendOutcome::Rejected { state, steps, .. } => {
                        self.refused = true;
                        (state, steps)
                    }
                };
            let head = state.head().map_or(0, |head| head.seq);
            for entry in entries {
                if let Some(seq) = entry["seq"].as_u64().filter(|seq| *seq <= head)
                    && let Some(link) = entry["link"].as_str() {
                        self.links.insert(seq, link.into());
                    }
            }
            for step in steps {
                let seq = match &step {
                    ChainStep::Batch(batch) => batch.seq,
                    ChainStep::Keyring { seq, .. } | ChainStep::Vetoed { seq } => *seq,
                };
                self.positions.insert(seq, step);
            }
            self.state = state;
            if head == after || head >= target {
                break;
            }
        }
        Ok(())
    }
    fn epoch_key(&self, epoch: u64) -> Option<Vec<u8>> {
        let authority = self.state.authority()?;
        let wrap = committed_wrap(
            &self.state,
            authority.epoch,
            &format!("device:{}", self.keyring.principal().id),
        )?;
        let material = own_material(
            self.keyring.as_ref(),
            &self.account,
            &wrap,
            &expected_history(authority, authority.epoch),
        )?;
        if material.epoch == epoch {
            Some(material.current.to_vec())
        } else {
            material
                .history
                .iter()
                .find(|key| key.epoch == epoch)
                .map(|key| key.key.to_vec())
        }
    }
}
struct Crypto(Rc<tokio::sync::Mutex<Chain>>);
fn hex_of_b64url(value: &str) -> String {
    URL_SAFE_NO_PAD.decode(value).map(|bytes| bytes.iter().map(|byte| format!("{byte:02x}")).collect()).unwrap_or_default()
}
impl Verifier for Crypto {
    fn verify_entry<'a>(
        &'a self,
        entry: &'a PulledEntry,
        bytes: &'a [u8],
    ) -> PortFuture<'a, Verdict> {
        Box::pin(async move {
            let mut chain = self.0.lock().await;
            chain.extend(entry.seq).await.map_err(EngineError::Other)?;
            let Some(position) = chain.positions.get(&entry.seq) else {
                return Ok(Verdict::Refused(RefusalCode::SequenceGap));
            };
            let batch = match position {
                ChainStep::Keyring { .. } => {
                    if entry.kind != EntryKind::Keyring { return Ok(Verdict::Refused(RefusalCode::ObjectBinding)); }
                    return Ok(Verdict::Ok { payload: BatchPayload { items: vec![] }, batch_id: format!("keyring:{}", entry.seq) });
                }
                ChainStep::Vetoed { .. } => {
                    if entry.kind != EntryKind::Batch { return Ok(Verdict::Refused(RefusalCode::ObjectBinding)); }
                    return Ok(Verdict::Ok { payload: BatchPayload { items: vec![] }, batch_id: entry.batch_id.clone().unwrap_or_else(|| format!("vetoed:{}", entry.seq)) });
                }
                ChainStep::Batch(batch) => batch,
            };
            if entry.kind != EntryKind::Batch || hex_of_b64url(&batch.digest) != entry.digest {
                return Ok(Verdict::Refused(RefusalCode::ContentHash));
            }
            let Some(key) = chain.epoch_key(batch.epoch) else {
                return Ok(Verdict::Refused(RefusalCode::Epoch));
            };
            let binding = BatchBinding {
                account: &chain.account,
                epoch: batch.epoch,
                author_id: &batch.author_id,
                signing_public_key: &batch.signing_public_key,
                digest: &batch.digest,
                kind: ObjectKind::Ops,
                chunk: None,
            };
            Ok(
                match open_batch(bytes, &binding, &key, &mut RejectionCounter::default()) {
                    OpenOutcome::Opened { batch_id, records } => Verdict::Ok {
                        batch_id,
                        payload: BatchPayload {
                            items: records
                                .into_iter()
                                .filter(|record| {
                                    record.r#type == 1 && record.space_id == chain.space
                                })
                                .map(|record| PayloadItem {
                                    key: record.item_key,
                                    ops: record.body,
                                })
                                .collect(),
                        },
                    },
                    OpenOutcome::Rejected { .. } => Verdict::Refused(RefusalCode::Malformed),
                },
            )
        })
    }
    fn verify_snapshot<'a>(
        &'a self,
        _: &'a SnapshotRef,
        _: &'a [Vec<u8>],
    ) -> PortFuture<'a, Verdict> {
        Box::pin(async { Ok(Verdict::Refused(RefusalCode::Malformed)) })
    }
}
impl Sealer for Crypto {
    fn seal<'a>(
        &'a self,
        batch_id: &'a str,
        base_seq: u64,
        payload: &'a BatchPayload,
    ) -> PortFuture<'a, SealedBatch> {
        Box::pin(async move {
            let mut chain = self.0.lock().await;
            chain.extend(base_seq).await.map_err(EngineError::Other)?;
            let authority = chain
                .state
                .authority()
                .ok_or_else(|| EngineError::Other("not_member".into()))?;
            if !authority
                .members
                .contains_key(&chain.keyring.principal().id)
            {
                return Err(EngineError::Other("not_member".into()));
            }
            let epoch = authority.epoch;
            let prior = chain
                .links
                .get(&base_seq)
                .ok_or_else(|| EngineError::Other("unverified_base".into()))?;
            let key = chain
                .epoch_key(epoch)
                .ok_or_else(|| EngineError::Other("no_epoch_key".into()))?;
            let id = URL_SAFE_NO_PAD
                .decode(batch_id)
                .map_err(|error| EngineError::Other(error.to_string()))?;
            let salt = [
                memory_core::support::random::random_uuid().replace('-', ""),
                memory_core::support::random::random_uuid().replace('-', ""),
            ]
            .concat();
            let salt: Vec<u8> = salt
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16).unwrap_or(0)
                })
                .collect();
            let records: Vec<_> = payload
                .items
                .iter()
                .map(|item| SyncObjectRecord {
                    r#type: 1,
                    space_id: chain.space.to_vec(),
                    item_key: item.key.clone(),
                    body: item.ops.clone(),
                })
                .collect();
            let bytes = build_batch(&BuildBatchInput {
                account: &chain.account,
                kind: ObjectKind::Ops,
                epoch,
                epoch_key: &key,
                keyring: chain.keyring.as_ref(),
                peers: &[],
                batch_id: &id,
                salt: &salt,
                created_at: chrono::Utc::now().timestamp_millis() as u64,
                chunk: None,
                records: &records,
            })
            .map_err(|error| EngineError::Other(error.to_string()))?;
            use sha2::{Digest, Sha256};
            let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
            let link = next_link(prior, &digest)
                .ok_or_else(|| EngineError::Other("unverified_base".into()))?;
            let record_sig = sign_record(
                chain.keyring.as_ref(),
                &SyncRecordFields {
                    account: chain.account.clone(),
                    base_seq,
                    digest,
                    epoch,
                    link: link.clone(),
                    keyring_link: None,
                },
            )
            .map_err(|error| EngineError::Other(error.to_string()))?;
            Ok(SealedBatch {
                bytes,
                epoch,
                link,
                record_sig,
            })
        })
    }
}
pub fn open_account_session(
    options: AccountSessionOptions,
) -> LocalFuture<Result<SyncSession, AccountSessionIdle>> {
    Box::pin(async move {
        let pin = load_trust_anchor(&options.pin_dir, &options.account_id)
            .map_err(|error| error.to_string())?;
        let anchor = match pin {
            PinLoad::Loaded { anchor } => anchor,
            PinLoad::Refused { reason, .. } => {
                return Ok(Err(if reason == "no_pin" {
                    AccountSessionIdle::NoPin
                } else {
                    AccountSessionIdle::CorruptPin
                }));
            }
        };
        let keyring = create_device_keyring(options.device_id.clone(), &options.material)
            .map_err(|error| error.to_string())?;
        let client = reqwest::Client::new();
        let mut chain = Chain {
            state: empty_chain(options.account_id.clone(), ChainAnchor::Pinned(anchor)),
            positions: BTreeMap::new(),
            links: BTreeMap::new(),
            refused: false,
            account: options.account_id,
            api: options.api.clone(),
            token: options.token.clone(),
            client: client.clone(),
            keyring: Box::new(keyring),
            space: options.space_id,
        };
        chain.extend(u64::MAX).await?;
        if !chain.state.authority().is_some_and(|authority| {
            authority
                .members
                .contains_key(&chain.keyring.principal().id)
        }) {
            return Ok(Err(AccountSessionIdle::NotMember));
        }
        let token = options.token;
        let transport = create_http_transport(HttpTransportOptions {
            base_url: options.api,
            client: Rc::new(Client(client)),
            token: Rc::new(move || {
                let token = token.clone();
                Box::pin(async move { token().await.map_err(EngineError::Other) })
            }),
        });
        let crypto = Rc::new(Crypto(Rc::new(tokio::sync::Mutex::new(chain))));
        Ok(Ok(SyncSession {
            transport: Rc::new(transport),
            verifier: crypto.clone(),
            sealer: crypto,
        }))
    })
}

#[cfg(test)]mod tests{
 use super::*;use memory_core::sync::keyring_port::generate_device_key_material;
 #[tokio::test]async fn missing_and_corrupt_trust_pins_open_no_network_session(){
 let root=tempfile::tempdir().unwrap();
 let options=||AccountSessionOptions{api:"http://127.0.0.1:1".into(),account_id:"account".into(),device_id:"device".into(),material:generate_device_key_material().unwrap().material,pin_dir:root.path().into(),space_id:[0;16],token:Rc::new(||panic!("trust refusal must not request a token"))};
 assert!(matches!(open_account_session(options()).await.unwrap(),Err(AccountSessionIdle::NoPin)));
 use sha2::{Digest,Sha256};let pin=root.path().join("trust-anchor").join(URL_SAFE_NO_PAD.encode(Sha256::digest(b"account")));std::fs::create_dir_all(&pin).unwrap();std::fs::write(pin.join("pin.json"),"invalid").unwrap();
 assert!(matches!(open_account_session(options()).await.unwrap(),Err(AccountSessionIdle::CorruptPin)));
 }
}
