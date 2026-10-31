//! Launch-owner presenter enrollment over the inherited bootstrap channel (senpi `mcp-presenter-enrollment.ts`).
//!
//! A v2 enrollment names an Ed25519 installation key, the launch nonce, the launch kind and (for a replacement)
//! the host it replaces. The host answers with a challenge transcript bound to its own incarnation and a fresh
//! nonce; only a signature over that exact transcript enrolls a principal.
use crate::mcp_presenter_wire::{BoxedPrivateStream, PrivateChannel};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410): the 32 key bytes follow.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
pub const ENROLLMENT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// `["native"]` or `["wsl", distro, epoch]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionRealm {
    Native,
    Wsl(String, String),
}
impl ExecutionRealm {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Native => json!(["native"]),
            Self::Wsl(distro, epoch) => json!(["wsl", distro, epoch]),
        }
    }
}

/// senpi `PresenterEnrollment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenterEnrollment {
    pub public_key: String,
    pub installation_key_id: String,
    pub backend_generation: String,
    pub launch_request_nonce: String,
    /// `initial` or `replacement`.
    pub launch_kind: String,
    pub predecessor_host_instance: Option<String>,
    pub execution_realm: ExecutionRealm,
    pub environment_id: String,
}
impl PresenterEnrollment {
    pub fn to_value(&self) -> Value {
        json!({
            "type": "enroll",
            "version": 2,
            "purpose": "mcp_form",
            "publicKey": self.public_key,
            "installationKeyId": self.installation_key_id,
            "backendGeneration": self.backend_generation,
            "launchRequestNonce": self.launch_request_nonce,
            "launchKind": self.launch_kind,
            "predecessorHostInstance": self.predecessor_host_instance,
            "executionRealm": self.execution_realm.to_value(),
            "environmentId": self.environment_id,
        })
    }
}

/// senpi `PresenterPrincipal`.
#[derive(Debug, Clone)]
pub struct PresenterPrincipal {
    pub id: String,
    pub key: ed25519_dalek::VerifyingKey,
    pub enrollment: PresenterEnrollment,
}

fn identity(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.encode_utf16().count() <= 256)
}
fn hex64(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The SPKI DER of a PEM Ed25519 public key (Node `createPublicKey(pem)` restricted to `ed25519`).
pub fn ed25519_spki_der(pem: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN PUBLIC KEY-----")?
        .strip_suffix("-----END PUBLIC KEY-----")?;
    let body: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    (der.len() == ED25519_SPKI_PREFIX.len() + 32 && der.starts_with(&ED25519_SPKI_PREFIX))
        .then_some(der)
}
pub fn ed25519_public_key(pem: &str) -> Option<ed25519_dalek::VerifyingKey> {
    let der = ed25519_spki_der(pem)?;
    let bytes: [u8; 32] = der[ED25519_SPKI_PREFIX.len()..].try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
}

/// Exact v2 boundary: no additional field can become an unsigned authorization input (senpi
/// `parsePresenterEnrollment`).
pub fn parse_presenter_enrollment(value: &Value) -> Option<PresenterEnrollment> {
    use sha2::Digest;
    let record = value.as_object()?;
    let public_key = record
        .get("publicKey")
        .and_then(Value::as_str)
        .filter(|key| key.encode_utf16().count() <= 1024)?;
    let launch_kind = record.get("launchKind").and_then(Value::as_str)?;
    if record.len() != 11
        || record.get("type").and_then(Value::as_str) != Some("enroll")
        || record.get("version").and_then(Value::as_f64) != Some(2.)
        || record.get("purpose").and_then(Value::as_str) != Some("mcp_form")
        || !matches!(launch_kind, "initial" | "replacement")
    {
        return None;
    }
    let installation_key_id = hex64(record.get("installationKeyId"))?;
    let backend_generation = identity(record.get("backendGeneration"))?;
    let launch_request_nonce = hex64(record.get("launchRequestNonce"))?;
    let environment_id = identity(record.get("environmentId"))?;
    let predecessor = record.get("predecessorHostInstance");
    let predecessor_host_instance = if launch_kind == "initial" {
        if predecessor != Some(&Value::Null) {
            return None;
        }
        None
    } else {
        Some(identity(predecessor)?.to_owned())
    };
    let realm = record.get("executionRealm")?.as_array()?;
    let execution_realm = match realm.as_slice() {
        [native] if native == "native" => ExecutionRealm::Native,
        [wsl, distro, epoch] if wsl == "wsl" => ExecutionRealm::Wsl(
            identity(Some(distro))?.to_owned(),
            identity(Some(epoch))?.to_owned(),
        ),
        _ => return None,
    };
    let der = ed25519_spki_der(public_key)?;
    ed25519_public_key(public_key)?;
    if hex::encode(sha2::Sha256::digest(&der)) != installation_key_id {
        return None;
    }
    Some(PresenterEnrollment {
        public_key: public_key.to_owned(),
        installation_key_id: installation_key_id.to_owned(),
        backend_generation: backend_generation.to_owned(),
        launch_request_nonce: launch_request_nonce.to_owned(),
        launch_kind: launch_kind.to_owned(),
        predecessor_host_instance,
        execution_realm,
        environment_id: environment_id.to_owned(),
    })
}

/// `JSON.stringify` of the fixed primitive tuple is the canonical signed encoding (senpi
/// `presenterEnrollmentTranscript`).
pub fn presenter_enrollment_transcript(
    request: &PresenterEnrollment,
    host_instance: &str,
    nonce: &str,
) -> String {
    json!([
        "mcp-presenter-enroll-v2",
        "mcp_form",
        request.installation_key_id,
        request.backend_generation,
        request.launch_request_nonce,
        request.launch_kind,
        request.predecessor_host_instance,
        request.execution_realm.to_value(),
        request.environment_id,
        host_instance,
        nonce,
        30000,
    ])
    .to_string()
}

pub(crate) fn random_hex32() -> String {
    let mut bytes = [0u8; 32];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

struct Pending {
    request: PresenterEnrollment,
    key: ed25519_dalek::VerifyingKey,
    transcript: String,
    deadline: tokio::time::Instant,
}

/// One bootstrap opportunity per host incarnation, including failed or cancelled attempts (senpi
/// `McpPresenterEnrollment`).
pub struct McpPresenterEnrollment {
    started: std::sync::atomic::AtomicBool,
    channel: Mutex<Option<PrivateChannel>>,
    pub nonces: Arc<Mutex<std::collections::BTreeSet<String>>>,
    host_instance: String,
    endpoint: String,
}
impl McpPresenterEnrollment {
    pub fn new(host_instance: &str, endpoint: &str) -> Self {
        Self {
            started: std::sync::atomic::AtomicBool::new(false),
            channel: Mutex::new(None),
            nonces: Arc::default(),
            host_instance: host_instance.to_owned(),
            endpoint: endpoint.to_owned(),
        }
    }

    pub fn bootstrap(
        &self,
        stream: BoxedPrivateStream,
        enrolled: impl FnOnce(PresenterPrincipal) + Send + 'static,
    ) {
        if self.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
            drop(stream);
            return;
        }
        let (host_instance, endpoint, nonces) = (
            self.host_instance.clone(),
            self.endpoint.clone(),
            Arc::clone(&self.nonces),
        );
        let mut pending: Option<Pending> = None;
        let mut consumed = false;
        let mut enrolled = Some(enrolled);
        let channel = PrivateChannel::open(
            stream,
            move |channel, value| {
                if consumed {
                    return;
                }
                let Some(attempt) = pending.take() else {
                    if value["type"] == "enroll" && value.get("version") != Some(&json!(2)) {
                        consumed = true;
                        channel.stop();
                        channel.end(&json!({"type":"unavailable","versions":[2]}));
                        return;
                    }
                    let Some(request) = parse_presenter_enrollment(&value) else {
                        channel.destroy();
                        return;
                    };
                    if request.predecessor_host_instance.as_deref() == Some(host_instance.as_str())
                    {
                        channel.destroy();
                        return;
                    }
                    let Some(key) = ed25519_public_key(&request.public_key) else {
                        channel.destroy();
                        return;
                    };
                    crate::mcp_form_dialog::lock(&nonces)
                        .insert(request.launch_request_nonce.clone());
                    let transcript =
                        presenter_enrollment_transcript(&request, &host_instance, &random_hex32());
                    channel.send(&json!({
                        "type": "challenge",
                        "version": 2,
                        "transcript": transcript,
                        "hostInstance": host_instance,
                    }));
                    pending = Some(Pending {
                        request,
                        key,
                        transcript,
                        deadline: tokio::time::Instant::now() + ENROLLMENT_DEADLINE,
                    });
                    return;
                };
                consumed = true;
                channel.stop();
                if tokio::time::Instant::now() >= attempt.deadline
                    || !crate::mcp_presenter_wire::signed(&value, &attempt.transcript, &attempt.key)
                {
                    channel.destroy();
                    return;
                }
                let principal = PresenterPrincipal {
                    id: uuid::Uuid::new_v4().to_string(),
                    key: attempt.key,
                    enrollment: attempt.request.clone(),
                };
                let answer = json!({
                    "type": "enrolled",
                    "version": 2,
                    "principalId": principal.id,
                    "hostInstance": host_instance,
                    "backendGeneration": attempt.request.backend_generation,
                    "launchRequestNonce": attempt.request.launch_request_nonce,
                    "endpoint": endpoint,
                });
                if let Some(enrolled) = enrolled.take() {
                    enrolled(principal);
                }
                channel.end(&answer);
            },
            true,
        );
        *crate::mcp_form_dialog::lock(&self.channel) = Some(channel);
    }

    pub fn dispose(&self) {
        if let Some(channel) = crate::mcp_form_dialog::lock(&self.channel).take() {
            channel.destroy();
        }
        crate::mcp_form_dialog::lock(&self.nonces).clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    pub fn signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[42; 32])
    }
    pub fn public_pem(key: &ed25519_dalek::SigningKey) -> String {
        use base64::Engine;
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(key.verifying_key().as_bytes());
        format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        )
    }
    /// senpi test fixture `enrollmentRequest(overrides)`.
    pub fn enrollment_request(overrides: Value) -> Value {
        use sha2::Digest;
        let key = signing_key();
        let pem = public_pem(&key);
        let der = ed25519_spki_der(&pem).unwrap_or_default();
        let mut request = json!({
            "type": "enroll",
            "version": 2,
            "purpose": "mcp_form",
            "publicKey": pem,
            "installationKeyId": hex::encode(sha2::Sha256::digest(&der)),
            "backendGeneration": "backend-4254",
            "launchRequestNonce": random_hex32(),
            "launchKind": "initial",
            "predecessorHostInstance": null,
            "executionRealm": ["native"],
            "environmentId": "environment-4254",
        });
        if let (Some(request), Some(overrides)) = (request.as_object_mut(), overrides.as_object()) {
            for (key, value) in overrides {
                if value.is_null() && key == "launchRequestNonce" {
                    request.remove(key);
                } else {
                    request.insert(key.clone(), value.clone());
                }
            }
        }
        request
    }
    pub fn enrollment_proof(transcript: &str) -> Value {
        json!({"type":"proof","signature":hex::encode(signing_key().sign(transcript.as_bytes()).to_bytes())})
    }

    /// One bootstrap peer: JSONL in and out of an in-memory stream.
    pub struct Peer {
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        lines: tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    }
    impl Peer {
        pub fn new(stream: tokio::io::DuplexStream) -> Self {
            use tokio::io::AsyncBufReadExt;
            let (read, write) = tokio::io::split(stream);
            Self {
                write,
                lines: tokio::io::BufReader::new(read).lines(),
            }
        }
        pub async fn send(&mut self, value: &Value) {
            use tokio::io::AsyncWriteExt;
            let _ = self.write.write_all(format!("{value}\n").as_bytes()).await;
        }
        /// Asserts the host closed the channel: the next read is EOF, not a frame and not the bound expiring.
        pub async fn eof(&mut self, case: impl std::fmt::Display) {
            let read =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.lines.next_line())
                    .await
                    .unwrap_or_else(|_| panic!("{case}: the host left the channel open"));
            assert!(
                matches!(read, Ok(None) | Err(_)),
                "{case}: expected EOF, got {read:?}"
            );
        }
        pub async fn next(&mut self) -> Option<Value> {
            let line =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.lines.next_line())
                    .await
                    .ok()?
                    .ok()??;
            serde_json::from_str(&line).ok()
        }
    }

    fn fixture() -> (
        McpPresenterEnrollment,
        Peer,
        Arc<Mutex<Vec<PresenterPrincipal>>>,
    ) {
        let enrollment = McpPresenterEnrollment::new("host-instance", "endpoint");
        let (peer, host) = tokio::io::duplex(1 << 16);
        let principals = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&principals);
        enrollment.bootstrap(Box::new(host), move |principal| {
            crate::mcp_form_dialog::lock(&sink).push(principal);
        });
        (enrollment, Peer::new(peer), principals)
    }

    #[tokio::test]
    async fn refuses_invalid_v2_authorization_fields() {
        for override_ in [
            json!({"launchRequestNonce": null}),
            json!({"launchRequestNonce": "A".repeat(64)}),
            json!({"purpose": "management"}),
            json!({"installationKeyId": "0".repeat(64)}),
            json!({"launchKind": "reconnect"}),
            json!({"launchKind": "replacement", "predecessorHostInstance": null}),
            json!({"predecessorHostInstance": "other"}),
            json!({"executionRealm": ["native", "extra"]}),
            json!({"environmentId": ""}),
            json!({"extraSignedField": "immediate"}),
            json!({"launchKind": "replacement", "predecessorHostInstance": "host-instance"}),
        ] {
            let (_enrollment, mut peer, _) = fixture();
            peer.send(&enrollment_request(override_.clone())).await;
            peer.eof(&override_).await;
        }
    }

    #[tokio::test]
    async fn signs_the_fixed_tuple_and_echoes_nonce_and_incarnation() {
        for (kind, predecessor) in [("initial", Value::Null), ("replacement", json!("older"))] {
            let (_enrollment, mut peer, principals) = fixture();
            let request = enrollment_request(
                json!({"launchKind": kind, "predecessorHostInstance": predecessor}),
            );
            peer.send(&request).await;
            let challenge = peer.next().await.unwrap();
            assert_eq!(challenge["type"], "challenge");
            assert_eq!(challenge["version"], 2);
            let transcript = challenge["transcript"].as_str().unwrap();
            let tuple: Value = serde_json::from_str(transcript).unwrap();
            assert_eq!(tuple[0], "mcp-presenter-enroll-v2");
            assert_eq!(tuple[2], request["installationKeyId"]);
            assert_eq!(tuple[4], request["launchRequestNonce"]);
            assert_eq!(tuple[5], kind);
            assert_eq!(tuple[6], predecessor);
            assert_eq!(tuple[7], json!(["native"]));
            assert_eq!(tuple[9], challenge["hostInstance"]);
            assert!(hex64(Some(&tuple[10])).is_some());
            assert_eq!(tuple[11], 30000);
            assert_eq!(tuple.to_string(), transcript);
            peer.send(&enrollment_proof(transcript)).await;
            let enrolled = peer.next().await.unwrap();
            assert_eq!(enrolled["type"], "enrolled");
            assert_eq!(
                enrolled["launchRequestNonce"],
                request["launchRequestNonce"]
            );
            assert_eq!(enrolled["hostInstance"], "host-instance");
            assert_eq!(enrolled["endpoint"], "endpoint");
            assert_eq!(crate::mcp_form_dialog::lock(&principals).len(), 1);
        }
    }

    #[tokio::test]
    async fn refuses_a_substituted_nonce_and_a_second_bootstrap() {
        let (enrollment, mut peer, principals) = fixture();
        peer.send(&enrollment_request(json!({}))).await;
        let challenge = peer.next().await.unwrap();
        let transcript = challenge["transcript"].as_str().unwrap();
        let mut tuple: Value = serde_json::from_str(transcript).unwrap();
        tuple[10] = random_hex32().into();
        peer.send(&enrollment_proof(&tuple.to_string())).await;
        peer.eof("substituted nonce").await;
        assert!(crate::mcp_form_dialog::lock(&principals).is_empty());
        let (second, host) = tokio::io::duplex(1024);
        enrollment.bootstrap(Box::new(host), |_| {});
        let mut second = Peer::new(second);
        second.send(&enrollment_request(json!({}))).await;
        second.eof("second bootstrap").await;
    }

    #[tokio::test]
    async fn an_older_version_is_told_which_versions_exist() {
        let (_enrollment, mut peer, _) = fixture();
        peer.send(&json!({"type":"enroll","version":1})).await;
        assert_eq!(
            peer.next().await,
            Some(json!({"type":"unavailable","versions":[2]}))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expires_a_challenge_at_its_deadline() {
        let (_enrollment, mut peer, principals) = fixture();
        peer.send(&enrollment_request(json!({}))).await;
        let challenge = peer.next().await.unwrap();
        tokio::time::advance(ENROLLMENT_DEADLINE).await;
        peer.send(&enrollment_proof(challenge["transcript"].as_str().unwrap()))
            .await;
        peer.eof("expired challenge").await;
        assert!(crate::mcp_form_dialog::lock(&principals).is_empty());
    }
}
