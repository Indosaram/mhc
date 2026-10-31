//! Successor enrollment bound to the replaced host instance (senpi `mcp-presenter-successor.ts`).
//!
//! A deferred handover is spawned by the old host, not by the CLI that already returned. So the authenticated
//! launch owner issues a fresh `replacement` permit at the actual launch, and the old host relays the
//! successor's challenge and proof between that owner and the new host.
use crate::mcp_form_dialog::lock;
use crate::mcp_presenter_enrollment::{
    PresenterPrincipal, parse_presenter_enrollment, presenter_enrollment_transcript,
};
use crate::mcp_presenter_wire::{BoxedPrivateStream, PrivateChannel};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub const SUCCESSOR_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The authenticated control stream that last proved itself, with its principal and backend generation.
#[derive(Clone)]
pub struct LaunchOwner {
    pub stream: PrivateChannel,
    pub principal: PresenterPrincipal,
    pub backend_generation: String,
}

type Receive = Box<dyn FnMut(&Map<String, Value>) + Send>;
struct Successor {
    owner: PrivateChannel,
    receive: Arc<Mutex<Receive>>,
    close: Arc<dyn Fn() + Send + Sync>,
}

/// senpi `McpPresenterSuccessors`.
pub struct McpPresenterSuccessors {
    pending: Arc<Mutex<BTreeMap<String, Successor>>>,
    predecessor: String,
    nonces: Arc<Mutex<BTreeSet<String>>>,
}

impl McpPresenterSuccessors {
    pub fn new(predecessor: &str, nonces: Arc<Mutex<BTreeSet<String>>>) -> Self {
        Self {
            pending: Arc::default(),
            predecessor: predecessor.to_owned(),
            nonces,
        }
    }

    /// Only an authenticated owner can issue a fresh permit at the actual deferred launch. Resolves to the stream
    /// the successor's bootstrap descriptor is relayed from, or `None` when no permit was issued.
    pub async fn prepare(&self, owner: LaunchOwner) -> Option<BoxedPrivateStream> {
        if owner.stream.is_destroyed() || !lock(&self.pending).is_empty() {
            return None;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let deadline = tokio::time::Instant::now() + SUCCESSOR_DEADLINE;
        let (resolve, resolved) = tokio::sync::oneshot::channel::<Option<BoxedPrivateStream>>();
        let resolve = Arc::new(Mutex::new(Some(resolve)));
        let endpoint: Arc<Mutex<Option<PrivateChannel>>> = Arc::default();
        let timer: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::default();
        let close: Arc<dyn Fn() + Send + Sync> = {
            let (pending, id, endpoint, resolve, timer) = (
                Arc::clone(&self.pending),
                id.clone(),
                Arc::clone(&endpoint),
                Arc::clone(&resolve),
                Arc::clone(&timer),
            );
            // Every lock is released before a callback runs: destroying the endpoint re-enters this close.
            Arc::new(move || {
                let timer = lock(&timer).take();
                if let Some(timer) = timer {
                    timer.abort();
                }
                lock(&pending).remove(&id);
                let endpoint = lock(&endpoint).take();
                if let Some(endpoint) = endpoint {
                    endpoint.destroy();
                }
                let resolve = lock(&resolve).take();
                if let Some(resolve) = resolve {
                    let _ = resolve.send(None);
                }
            })
        };
        {
            let close = Arc::clone(&close);
            *lock(&timer) = Some(tokio::spawn(async move {
                tokio::time::sleep(SUCCESSOR_DEADLINE).await;
                close();
            }));
        }
        let proof: Arc<Mutex<Option<PrivateChannel>>> = Arc::default();
        let receive: Receive = {
            let (owner, predecessor, nonces, close, endpoint, resolve, proof, id) = (
                owner.clone(),
                self.predecessor.clone(),
                Arc::clone(&self.nonces),
                Arc::clone(&close),
                Arc::clone(&endpoint),
                Arc::clone(&resolve),
                Arc::clone(&proof),
                id.clone(),
            );
            Box::new(move |value| {
                if tokio::time::Instant::now() >= deadline {
                    return close();
                }
                let kind = value.get("type").and_then(Value::as_str);
                if kind == Some("mcp_presenter_successor_proof")
                    && let Some(signature) = value.get("signature").and_then(Value::as_str)
                {
                    let Some(child) = lock(&proof).take() else {
                        return close();
                    };
                    child.send(&json!({"type":"proof","signature":signature}));
                    return;
                }
                if kind != Some("mcp_presenter_successor_authorize") || lock(&endpoint).is_some() {
                    return close();
                }
                let previous = &owner.principal.enrollment;
                let request = value
                    .get("enrollment")
                    .and_then(parse_presenter_enrollment)
                    .filter(|request| {
                        request.launch_kind == "replacement"
                            && request.predecessor_host_instance.as_deref()
                                == Some(predecessor.as_str())
                            && request.backend_generation == owner.backend_generation
                            && request.installation_key_id == previous.installation_key_id
                            && request.environment_id == previous.environment_id
                            && request.execution_realm == previous.execution_realm
                            && !lock(&nonces).contains(&request.launch_request_nonce)
                    });
                let Some(request) = request else {
                    return close();
                };
                lock(&nonces).insert(request.launch_request_nonce.clone());
                let (child_end, host_end) = tokio::io::duplex(64 * 1024);
                let mut incarnation: Option<String> = None;
                let (
                    frame_close,
                    frame_owner,
                    frame_proof,
                    frame_predecessor,
                    frame_request,
                    frame_id,
                ) = (
                    Arc::clone(&close),
                    owner.stream.clone(),
                    Arc::clone(&proof),
                    predecessor.clone(),
                    request.clone(),
                    id.clone(),
                );
                let channel = PrivateChannel::open(
                    Box::new(host_end),
                    move |child, frame| {
                        if tokio::time::Instant::now() >= deadline || !frame.is_object() {
                            return frame_close();
                        }
                        let awaiting_proof = lock(&frame_proof).is_some();
                        if frame["type"] == "challenge"
                            && incarnation.is_none()
                            && frame["version"] == 2
                        {
                            let (Some(host), Some(transcript)) =
                                (frame["hostInstance"].as_str(), frame["transcript"].as_str())
                            else {
                                return frame_close();
                            };
                            if host == frame_predecessor {
                                return frame_close();
                            }
                            let nonce = serde_json::from_str::<Value>(transcript)
                                .ok()
                                .and_then(|tuple| {
                                    tuple.get(10).and_then(Value::as_str).map(str::to_owned)
                                })
                                .filter(|nonce| {
                                    nonce.len() == 64
                                        && nonce
                                            .bytes()
                                            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                                });
                            let Some(nonce) = nonce else {
                                return frame_close();
                            };
                            if transcript
                                != presenter_enrollment_transcript(&frame_request, host, &nonce)
                            {
                                return frame_close();
                            }
                            incarnation = Some(host.to_owned());
                            *lock(&frame_proof) = Some(child.clone());
                            frame_owner.send(&json!({
                                "type": "mcp_presenter_successor_challenge",
                                "id": frame_id,
                                "challenge": frame,
                            }));
                        } else if frame["type"] == "enrolled"
                            && incarnation.is_some()
                            && !awaiting_proof
                            && frame["version"] == 2
                            && frame["hostInstance"].as_str() == incarnation.as_deref()
                            && frame["launchRequestNonce"].as_str()
                                == Some(frame_request.launch_request_nonce.as_str())
                            && frame["backendGeneration"].as_str()
                                == Some(frame_request.backend_generation.as_str())
                        {
                            frame_owner.send(&json!({
                                "type": "mcp_presenter_successor_enrolled",
                                "id": frame_id,
                                "enrollment": frame,
                            }));
                            frame_close();
                        } else {
                            frame_close();
                        }
                    },
                    false,
                );
                {
                    let close = Arc::clone(&close);
                    channel.on_close(move || close());
                }
                channel.send(&request.to_value());
                *lock(&endpoint) = Some(channel);
                let resolve = lock(&resolve).take();
                if let Some(resolve) = resolve {
                    let _ = resolve.send(Some(Box::new(child_end) as BoxedPrivateStream));
                }
            })
        };
        lock(&self.pending).insert(
            id.clone(),
            Successor {
                owner: owner.stream.clone(),
                receive: Arc::new(Mutex::new(receive)),
                close,
            },
        );
        owner.stream.send(&json!({
            "type": "mcp_presenter_successor_prepare",
            "id": id,
            "version": 2,
            "launchKind": "replacement",
            "predecessorHostInstance": self.predecessor,
            "backendGeneration": owner.backend_generation,
        }));
        resolved.await.ok().flatten()
    }

    /// Consumes the owner's successor frames; anything addressed to another owner's permit destroys the stream.
    pub fn receive(&self, stream: &PrivateChannel, value: &Map<String, Value>) -> bool {
        let kind = value.get("type").and_then(Value::as_str);
        if !matches!(
            kind,
            Some("mcp_presenter_successor_authorize" | "mcp_presenter_successor_proof")
        ) {
            return false;
        }
        let receive = value.get("id").and_then(Value::as_str).and_then(|id| {
            lock(&self.pending)
                .get(id)
                .filter(|pending| pending.owner == *stream)
                .map(|pending| Arc::clone(&pending.receive))
        });
        match receive {
            Some(receive) => (*lock(&receive))(value),
            None => stream.destroy(),
        }
        true
    }

    pub fn revoke(&self, stream: &PrivateChannel) {
        let closes: Vec<_> = lock(&self.pending)
            .values()
            .filter(|pending| pending.owner == *stream)
            .map(|pending| Arc::clone(&pending.close))
            .collect();
        for close in closes {
            close();
        }
    }

    pub fn dispose(&self) {
        let closes: Vec<_> = lock(&self.pending)
            .values()
            .map(|pending| Arc::clone(&pending.close))
            .collect();
        for close in closes {
            close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_presenter_enrollment::tests::{
        Peer, enrollment_proof, enrollment_request, signing_key,
    };

    struct Fixture {
        original: crate::mcp_presenter_enrollment::PresenterEnrollment,
        owner: LaunchOwner,
        peer: Peer,
        successors: Arc<McpPresenterSuccessors>,
    }
    fn fixture() -> Fixture {
        let original = parse_presenter_enrollment(&enrollment_request(json!({}))).unwrap();
        let (peer, host) = tokio::io::duplex(1 << 16);
        let stream = PrivateChannel::open(Box::new(host), |_, _| {}, false);
        let owner = LaunchOwner {
            stream,
            backend_generation: original.backend_generation.clone(),
            principal: PresenterPrincipal {
                id: "principal".into(),
                key: signing_key().verifying_key(),
                enrollment: original.clone(),
            },
        };
        let nonces = Arc::new(Mutex::new(BTreeSet::from([original
            .launch_request_nonce
            .clone()])));
        Fixture {
            original,
            owner,
            peer: Peer::new(peer),
            successors: Arc::new(McpPresenterSuccessors::new("predecessor", nonces)),
        }
    }
    fn authorize(id: &Value, enrollment: Value) -> Map<String, Value> {
        json!({"type":"mcp_presenter_successor_authorize","id":id,"enrollment":enrollment})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn refuses_a_substituted_or_reused_successor_field_before_launch() {
        for field in [
            "nonce",
            "predecessor",
            "backend",
            "realm",
            "environment",
            "kind",
        ] {
            let mut f = fixture();
            let successors = Arc::clone(&f.successors);
            let owner = f.owner.clone();
            let pending = tokio::spawn(async move { successors.prepare(owner).await.is_some() });
            let prepare = f.peer.next().await.unwrap();
            assert_eq!(prepare["type"], "mcp_presenter_successor_prepare");
            assert_eq!(prepare["predecessorHostInstance"], "predecessor");
            let mut overrides =
                json!({"launchKind":"replacement","predecessorHostInstance":"predecessor"});
            match field {
                "nonce" => {
                    overrides["launchRequestNonce"] = f.original.launch_request_nonce.clone().into()
                }
                "predecessor" => overrides["predecessorHostInstance"] = "other".into(),
                "backend" => overrides["backendGeneration"] = "other".into(),
                "realm" => overrides["executionRealm"] = json!(["wsl", "distro", "epoch"]),
                "environment" => overrides["environmentId"] = "other".into(),
                _ => overrides = json!({"launchKind":"initial","predecessorHostInstance":null}),
            }
            f.successors.receive(
                &f.owner.stream,
                &authorize(&prepare["id"], enrollment_request(overrides)),
            );
            assert!(!pending.await.unwrap(), "{field}");
        }
    }

    #[tokio::test]
    async fn revokes_a_prepared_launch_when_its_owner_disconnects() {
        let mut f = fixture();
        let successors = Arc::clone(&f.successors);
        let owner = f.owner.clone();
        let pending = tokio::spawn(async move { successors.prepare(owner).await.is_some() });
        f.peer.next().await.unwrap();
        f.successors.revoke(&f.owner.stream);
        assert!(!pending.await.unwrap());
    }

    #[tokio::test]
    async fn relays_a_fresh_successor_proof_once_and_matches_the_acknowledgement() {
        use crate::mcp_presenter_enrollment::McpPresenterEnrollment;
        let mut f = fixture();
        let successors = Arc::clone(&f.successors);
        let owner = f.owner.clone();
        let pending = tokio::spawn(async move { successors.prepare(owner).await });
        let prepare = f.peer.next().await.unwrap();
        let request = enrollment_request(
            json!({"launchKind":"replacement","predecessorHostInstance":"predecessor"}),
        );
        f.successors
            .receive(&f.owner.stream, &authorize(&prepare["id"], request.clone()));
        let endpoint = pending.await.unwrap().unwrap();
        // The successor host's own one-shot enrollment reads the relayed permit.
        let successor = McpPresenterEnrollment::new("successor-host", "successor-endpoint");
        let (enrolled_tx, enrolled_rx) = tokio::sync::oneshot::channel();
        successor.bootstrap(endpoint, move |principal| {
            let _ = enrolled_tx.send(principal.enrollment.launch_request_nonce);
        });
        let challenge = f.peer.next().await.unwrap();
        assert_eq!(challenge["type"], "mcp_presenter_successor_challenge");
        let transcript = challenge["challenge"]["transcript"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut proof = enrollment_proof(&transcript);
        proof["type"] = "mcp_presenter_successor_proof".into();
        proof["id"] = prepare["id"].clone();
        f.successors
            .receive(&f.owner.stream, proof.as_object().unwrap());
        let enrolled = f.peer.next().await.unwrap();
        assert_eq!(enrolled["type"], "mcp_presenter_successor_enrolled");
        assert_eq!(enrolled["enrollment"]["hostInstance"], "successor-host");
        assert_eq!(
            enrolled_rx.await.unwrap().as_str(),
            request["launchRequestNonce"].as_str().unwrap()
        );
        assert!(lock(&f.successors.pending).is_empty());
    }
}
