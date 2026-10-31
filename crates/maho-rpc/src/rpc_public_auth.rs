//! The public endpoint's per-connection filter (senpi `rpc-public-auth.ts`, #3795): it filters public input before
//! it can reach either the supervisor's proxy or the bare host's router. A client proves the endpoint key with the
//! `get_protocol_info` challenge and `authenticate`; under enforcement (`SENPI_RPC_REQUIRE_TOKEN=1`) anything else is
//! refused, and in grace a keyless client is served with one deprecation line.
//!
//! `public_rpc_stream` is senpi's `stream` view: the admitted connection as one duplex. The router reads only frames
//! this filter released and its writes pass through `response`, so the challenge answer gains its proof.
#![cfg(unix)]
use crate::jsonl::{JsonlLineReader, LineRecord, MAX_RPC_LINE_CHARACTERS, serialize_json_line};
use crate::rpc_auth_proof::{
    RPC_AUTH_CAPABILITY, is_rpc_auth_hex, rpc_auth_proof, rpc_proof_matches,
};
use crate::rpc_public_key::RpcPublicEndpoint;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const PREAUTH_BYTES: usize = 8 * 1024;

struct Challenge {
    id: Option<Value>,
    client: String,
    server: String,
}

/// What one released public line asks of the transport.
#[derive(Debug, PartialEq, Eq)]
pub enum PublicLine {
    /// Hand the line (no LF) to the router.
    Forward(String),
    /// Write this record straight to the client (the `authenticate` success).
    Reply(String),
    /// Write this refusal, then end the connection.
    Refuse(String),
    /// The connection is already failed or dropped: ignore.
    Ignore,
}

pub struct PublicRpcConnection {
    endpoint: Arc<RpcPublicEndpoint>,
    /// Enforcement: refuse an unauthenticated client. Off by default for one release of logged grace.
    require_token: bool,
    bytes: usize,
    active: bool,
    failed: bool,
    first: bool,
    challenge: Option<Challenge>,
    protocol_pending: bool,
}

impl PublicRpcConnection {
    pub fn new(endpoint: Arc<RpcPublicEndpoint>, require_token: bool) -> Self {
        Self {
            endpoint,
            require_token,
            bytes: 0,
            active: false,
            failed: false,
            first: true,
            challenge: None,
            protocol_pending: false,
        }
    }

    /// senpi's default: `SENPI_RPC_REQUIRE_TOKEN === "1"`.
    pub fn require_token_from_env() -> bool {
        std::env::var("SENPI_RPC_REQUIRE_TOKEN").as_deref() == Ok("1")
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Counts one LF segment of raw input; `Err` is the drop reason when enforcement's pre-auth budget is exceeded.
    /// Only enforcement bounds pre-auth input: in grace a keyless legacy client is served whatever its first frame.
    pub fn count_segment(&mut self, length: usize) -> Result<(), &'static str> {
        if !self.active {
            self.bytes += length;
        }
        if !self.active && self.require_token && self.bytes > PREAUTH_BYTES {
            return Err("pre-auth budget exceeded");
        }
        Ok(())
    }

    /// The pre-auth deadline: enforcing drops; grace serves a silent or slow legacy client unless a challenge is in
    /// flight. `Err` is the drop reason.
    pub fn deadline(&mut self) -> Result<(), &'static str> {
        if self.require_token {
            return Err("pre-auth deadline");
        }
        if !self.active && !self.failed && self.challenge.is_none() {
            self.grace();
        }
        Ok(())
    }

    /// The peer went away.
    pub fn closed(&mut self) {
        if !self.active && !self.failed && !self.require_token && !self.first {
            self.grace();
        }
    }

    /// An oversized line: `true` destroys an admitted connection, `false` means the caller drops it.
    pub fn oversized(&self) -> bool {
        self.active
    }

    pub fn line(&mut self, line: &str) -> PublicLine {
        if self.failed {
            return PublicLine::Ignore;
        }
        // Malformed JSON is left to the existing router only after admission.
        let request: Option<Map<String, Value>> = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.as_object().cloned());
        let kind = request
            .as_ref()
            .and_then(|request| request.get("type"))
            .and_then(Value::as_str);
        if !self.active {
            if self.first && kind == Some("get_protocol_info") {
                self.first = false;
                self.protocol_pending = true;
                let nonce = request
                    .as_ref()
                    .and_then(|request| request.get("auth"))
                    .and_then(Value::as_object)
                    .and_then(|auth| auth.get("client_nonce"));
                if let Some(nonce) = nonce {
                    let Some(nonce) = nonce.as_str().filter(|nonce| is_rpc_auth_hex(nonce)) else {
                        return self.refuse(request.as_ref(), "auth_failed");
                    };
                    let mut server = [0u8; 32];
                    if getrandom::fill(&mut server).is_err() {
                        return self.refuse(request.as_ref(), "auth_failed");
                    }
                    self.challenge = Some(Challenge {
                        id: request
                            .as_ref()
                            .and_then(|request| request.get("id"))
                            .cloned(),
                        client: nonce.to_owned(),
                        server: hex::encode(server),
                    });
                } else if self.require_token {
                    // Wire contract frame 5: under enforcement a keyless get_protocol_info is refused too.
                    return self.refuse(request.as_ref(), "auth_required");
                } else {
                    self.grace();
                }
                return PublicLine::Forward(line.to_owned());
            }
            self.first = false;
            if kind == Some("authenticate") {
                let proven = self.challenge.as_ref().is_some_and(|challenge| {
                    rpc_proof_matches(
                        &rpc_auth_proof(
                            &self.endpoint.key,
                            "client",
                            &[
                                &self.endpoint.launch_id,
                                &challenge.server,
                                &challenge.client,
                            ],
                        ),
                        request
                            .as_ref()
                            .and_then(|request| request.get("client_proof")),
                    )
                });
                if !proven {
                    return self.refuse(request.as_ref(), "auth_failed");
                }
                self.active = true;
                let id = request
                    .as_ref()
                    .and_then(|request| request.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                return PublicLine::Reply(line_of(
                    &json!({"id":id,"type":"response","command":"authenticate","success":true}),
                ));
            }
            if self.require_token {
                return self.refuse(request.as_ref(), "auth_required");
            }
            self.grace();
        }
        PublicLine::Forward(line.to_owned())
    }

    fn grace(&mut self) {
        self.active = true;
        eprintln!(
            "senpi rpc host: served a client that did not prove the endpoint key on {} (deprecated, refused from the next release; see docs/rpc.md#authentication)",
            self.endpoint.endpoint()
        );
    }

    /// Every pre-auth drop says why on stderr, by reason code only: never a frame, a nonce or key material.
    pub fn drop_connection(&mut self, reason: &str) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.log_drop(reason);
    }

    fn log_drop(&self, reason: &str) {
        eprintln!(
            "senpi rpc host: dropped an unauthenticated connection on {}: {reason}",
            self.endpoint.endpoint()
        );
    }

    fn refuse(&mut self, request: Option<&Map<String, Value>>, code: &str) -> PublicLine {
        self.failed = true;
        self.log_drop(code);
        let mut response = Map::new();
        if let Some(id) = request.and_then(|request| request.get("id")) {
            response.insert("id".into(), id.clone());
        }
        response.insert("type".into(), "response".into());
        response.insert(
            "command".into(),
            request
                .and_then(|request| request.get("type"))
                .cloned()
                .unwrap_or_else(|| "parse".into()),
        );
        response.insert("success".into(), false.into());
        response.insert("error".into(), code.into());
        response.insert("errorCode".into(), code.into());
        PublicLine::Refuse(line_of(&Value::Object(response)))
    }

    /// Writer/proxy batches of whole JSONL records; ordinary authenticated bytes stay untouched. Before admission
    /// only the challenge answer leaves an enforcing host; a grace host keeps delivering records.
    pub fn response(&mut self, chunk: &str) -> String {
        if self.failed {
            return String::new();
        }
        if self.active && !self.protocol_pending {
            return chunk.to_owned();
        }
        let mut output = String::new();
        for line in chunk.split('\n') {
            if line.is_empty() {
                continue;
            }
            let record = serde_json::from_str::<Value>(line).ok();
            let record = record.as_ref().and_then(Value::as_object);
            if let Some(record) = record.filter(|record| {
                record.get("type") == Some(&json!("response"))
                    && record.get("command") == Some(&json!("get_protocol_info"))
                    && record.get("success") == Some(&Value::Bool(true))
            }) {
                self.protocol_pending = false;
                let mut data = record
                    .get("data")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let mut capabilities = data
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if !capabilities.contains(&json!(RPC_AUTH_CAPABILITY)) {
                    capabilities.push(RPC_AUTH_CAPABILITY.into());
                }
                let mut deduped: Vec<Value> = Vec::new();
                for capability in capabilities {
                    if !deduped.contains(&capability) {
                        deduped.push(capability);
                    }
                }
                data.insert("capabilities".into(), Value::Array(deduped));
                if let Some(challenge) = self
                    .challenge
                    .as_ref()
                    .filter(|challenge| record.get("id") == challenge.id.as_ref())
                {
                    data.insert(
                        "auth".into(),
                        json!({
                            "launch_id": self.endpoint.launch_id,
                            "server_nonce": challenge.server,
                            "server_proof": rpc_auth_proof(
                                &self.endpoint.key,
                                "server",
                                &[&self.endpoint.launch_id, &challenge.client, &challenge.server],
                            ),
                        }),
                    );
                }
                let mut record = record.clone();
                record.insert("data".into(), Value::Object(data));
                output.push_str(&line_of(&Value::Object(record)));
            } else if self.active || !self.require_token {
                output.push_str(line);
                output.push('\n');
            }
        }
        output
    }
}

fn line_of(value: &Value) -> String {
    serialize_json_line(value).unwrap_or_else(|_| format!("{value}\n"))
}

/// senpi `PublicRpcConnection.stream`: drives the filter on `socket` and returns the admitted duplex. The router
/// reads released frames from it; whatever it writes passes `response` on its way to the client. Released frames
/// already queued when the peer goes away are still delivered, then the view ends.
pub fn public_rpc_stream<S>(
    socket: S,
    endpoint: Arc<RpcPublicEndpoint>,
    require_token: bool,
) -> tokio::io::DuplexStream
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (view, far) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut auth = PublicRpcConnection::new(endpoint, require_token);
        let (mut socket_read, mut socket_write) = tokio::io::split(socket);
        let (mut far_read, mut far_write) = tokio::io::split(far);
        let mut lines = match JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS) {
            Ok(lines) => lines,
            Err(_) => return,
        };
        let deadline = tokio::time::sleep(std::time::Duration::from_millis(
            crate::socket_transport::SOCKET_HANDSHAKE_TIMEOUT_MS,
        ));
        tokio::pin!(deadline);
        let mut deadline_done = false;
        let mut input_open = true;
        let mut outgoing: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 16 * 1024];
        let mut back = vec![0u8; 16 * 1024];
        'pump: loop {
            tokio::select! {
                () = &mut deadline, if !deadline_done => {
                    deadline_done = true;
                    if auth.active { continue; }
                    if let Err(reason) = auth.deadline() { auth.drop_connection(reason); break 'pump; }
                }
                read = socket_read.read(&mut chunk), if input_open => {
                    let count = match read { Ok(count) => count, Err(_) => 0 };
                    if count == 0 {
                        auth.closed();
                        input_open = false;
                        for record in lines.finish() {
                            if let LineRecord::Line(line) = record
                                && let PublicLine::Forward(line) = auth.line(&line)
                                && far_write.write_all(format!("{line}\n").as_bytes()).await.is_err()
                            { break 'pump; }
                        }
                        let _ = far_write.shutdown().await;
                        continue;
                    }
                    // Feed a coalesced chunk one LF segment at a time: proof verification can remove the 8 KiB
                    // budget before a large authenticated command in that same chunk.
                    for segment in chunk[..count].split_inclusive(|byte| *byte == b'\n') {
                        if auth.failed() { break 'pump; }
                        if let Err(reason) = auth.count_segment(segment.len()) {
                            auth.drop_connection(reason);
                            break 'pump;
                        }
                        for record in lines.push(segment) {
                            match record {
                                LineRecord::Oversized => {
                                    if !auth.oversized() { auth.drop_connection("first frame over the line limit"); }
                                    break 'pump;
                                }
                                LineRecord::Line(line) => match auth.line(&line) {
                                    PublicLine::Forward(line) => {
                                        if far_write.write_all(format!("{line}\n").as_bytes()).await.is_err() { break 'pump; }
                                    }
                                    PublicLine::Reply(reply) => {
                                        if socket_write.write_all(reply.as_bytes()).await.is_err() { break 'pump; }
                                    }
                                    PublicLine::Refuse(reply) => {
                                        let _ = socket_write.write_all(reply.as_bytes()).await;
                                        let _ = socket_write.shutdown().await;
                                        break 'pump;
                                    }
                                    PublicLine::Ignore => {}
                                },
                            }
                        }
                    }
                }
                read = far_read.read(&mut back) => {
                    let count = match read { Ok(count) => count, Err(_) => 0 };
                    if count == 0 {
                        let rest = std::mem::take(&mut outgoing);
                        if !rest.is_empty() {
                            let output = auth.response(&String::from_utf8_lossy(&rest));
                            let _ = socket_write.write_all(output.as_bytes()).await;
                        }
                        let _ = socket_write.shutdown().await;
                        break 'pump;
                    }
                    // Bytes, not text: a UTF-8 sequence split across reads stays whole until its record completes.
                    outgoing.extend_from_slice(&back[..count]);
                    if let Some(end) = outgoing.iter().rposition(|byte| *byte == b'\n') {
                        let complete: Vec<u8> = outgoing.drain(..=end).collect();
                        let output = auth.response(&String::from_utf8_lossy(&complete));
                        if !output.is_empty() && socket_write.write_all(output.as_bytes()).await.is_err() { break 'pump; }
                    }
                }
            }
        }
    });
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn endpoint() -> (tempfile::TempDir, Arc<RpcPublicEndpoint>) {
        let temp = tempfile::tempdir().expect("dir");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let endpoint = RpcPublicEndpoint::new(temp.path(), "/tmp/host.sock").expect("endpoint");
        (temp, Arc::new(endpoint))
    }

    #[test]
    fn a_proven_challenge_admits_and_the_protocol_reply_carries_the_server_proof() {
        let (_temp, endpoint) = endpoint();
        let mut auth = PublicRpcConnection::new(endpoint.clone(), true);
        let client = "0a".repeat(32);
        let first =
            json!({"id":"p","type":"get_protocol_info","auth":{"client_nonce":client}}).to_string();
        assert!(matches!(auth.line(&first), PublicLine::Forward(_)));
        let reply = auth.response("{\"id\":\"p\",\"type\":\"response\",\"command\":\"get_protocol_info\",\"success\":true,\"data\":{\"capabilities\":[]}}\n");
        let reply: Value = serde_json::from_str(reply.trim_end()).expect("json");
        assert_eq!(reply["data"]["capabilities"], json!(["socket_auth"]));
        let server = reply["data"]["auth"]["server_nonce"]
            .as_str()
            .expect("nonce")
            .to_owned();
        assert_eq!(
            reply["data"]["auth"]["server_proof"],
            json!(rpc_auth_proof(
                &endpoint.key,
                "server",
                &[&endpoint.launch_id, &client, &server]
            ))
        );
        let proof = rpc_auth_proof(
            &endpoint.key,
            "client",
            &[&endpoint.launch_id, &server, &client],
        );
        let authenticate = json!({"id":"a","type":"authenticate","client_proof":proof}).to_string();
        assert_eq!(
            auth.line(&authenticate),
            PublicLine::Reply("{\"id\":\"a\",\"type\":\"response\",\"command\":\"authenticate\",\"success\":true}\n".into())
        );
        assert_eq!(auth.response("{\"x\":1}\n"), "{\"x\":1}\n");
    }

    #[test]
    fn enforcement_refuses_keyless_and_wrong_proofs_and_bounds_pre_auth_input() {
        let (_temp, endpoint) = endpoint();
        let mut auth = PublicRpcConnection::new(endpoint.clone(), true);
        assert_eq!(
            auth.line(r#"{"id":"p","type":"get_protocol_info"}"#),
            PublicLine::Refuse("{\"id\":\"p\",\"type\":\"response\",\"command\":\"get_protocol_info\",\"success\":false,\"error\":\"auth_required\",\"errorCode\":\"auth_required\"}\n".into())
        );
        assert_eq!(auth.response("{\"x\":1}\n"), "");
        let mut auth = PublicRpcConnection::new(endpoint.clone(), true);
        assert!(matches!(
            auth.line(r#"{"id":"a","type":"authenticate","client_proof":"00"}"#),
            PublicLine::Refuse(_)
        ));
        let mut auth = PublicRpcConnection::new(endpoint, true);
        assert_eq!(auth.count_segment(9000), Err("pre-auth budget exceeded"));
    }

    #[test]
    fn grace_serves_a_keyless_client_and_its_records() {
        let (_temp, endpoint) = endpoint();
        let mut auth = PublicRpcConnection::new(endpoint, false);
        assert!(auth.count_segment(9000).is_ok());
        assert_eq!(
            auth.line(r#"{"id":"l","type":"list_sessions"}"#),
            PublicLine::Forward(r#"{"id":"l","type":"list_sessions"}"#.into())
        );
        assert_eq!(auth.response("{\"x\":1}\n"), "{\"x\":1}\n");
    }
}
