//! The sealed internal hop's handshake (senpi `internal-channel.ts`, senpi#3795): a challenge/response over
//! newline-delimited JSON frames, proven with the wire contract v1.1 HMAC under launch id `internal`, then every byte
//! after it travels in the directional AES-GCM record stream. There is no legacy or downgrade path on this hop.
//!
//! The host (server) side guards its pre-auth reads with the 8 KiB / 2 s budget, fragmented frames and bytes
//! coalesced after authentication included. The supervisor-side client bounds a line by the normal RPC ceiling; its
//! deadline is the caller's: the readiness probe and the activity observer pass one, while the proxy forwarding a
//! public connection passes none, so a host whose event loop stalls still serves the connections made meanwhile.
use crate::internal_record_stream::{
    InternalRecordStream, InternalRole, create_internal_record_stream,
};
use crate::jsonl::MAX_RPC_LINE_CHARACTERS;
use crate::rpc_auth_proof::{is_rpc_auth_hex, rpc_auth_proof, rpc_proof_matches};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type Frame = Map<String, Value>;
const INTERNAL_LAUNCH_ID: &str = "internal";
const HOST_PRE_AUTH_MAX_BYTES: usize = 8192;
const HOST_PRE_AUTH_TIMEOUT_MS: u64 = 2000;

/// The refusal or failure code of a handshake (`auth_required`, `auth_failed`, `host_identity_unproven`,
/// `internal_auth_timeout`, `internal_auth_oversized`, `internal_hop_lost`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InternalChannelError(pub &'static str);

/// One HMAC for both endpoints; the hop's launch id is "internal".
fn proof(key: &[u8], label: &str, first: &str, second: &str) -> String {
    rpc_auth_proof(key, label, &[INTERNAL_LAUNCH_ID, first, second])
}

fn random_nonce() -> Result<String, InternalChannelError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| InternalChannelError("internal_hop_lost"))?;
    Ok(hex::encode(bytes))
}

struct Handshake<S> {
    socket: S,
    buffer: Vec<u8>,
    read_bytes: usize,
    max_bytes: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Handshake<S> {
    async fn read(&mut self) -> Result<Frame, InternalChannelError> {
        let mut scanned = 0;
        loop {
            if let Some(newline) = self.buffer[scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                let used = scanned + newline + 1;
                self.read_bytes += newline + 1;
                if self.read_bytes > self.max_bytes {
                    return Err(InternalChannelError("internal_auth_oversized"));
                }
                let line: Vec<u8> = self.buffer.drain(..used).collect();
                return serde_json::from_slice::<Value>(&line)
                    .ok()
                    .and_then(|value| match value {
                        Value::Object(frame) => Some(frame),
                        _ => None,
                    })
                    .ok_or(InternalChannelError("auth_failed"));
            }
            self.read_bytes += self.buffer.len() - scanned;
            if self.read_bytes > self.max_bytes {
                return Err(InternalChannelError("internal_auth_oversized"));
            }
            scanned = self.buffer.len();
            let mut chunk = [0u8; 4096];
            let count = self
                .socket
                .read(&mut chunk)
                .await
                .map_err(|_| InternalChannelError("internal_hop_lost"))?;
            if count == 0 {
                return Err(InternalChannelError("internal_hop_lost"));
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }

    async fn send(&mut self, frame: Value) -> Result<(), InternalChannelError> {
        let mut line = frame.to_string();
        line.push('\n');
        self.socket
            .write_all(line.as_bytes())
            .await
            .map_err(|_| InternalChannelError("internal_hop_lost"))?;
        self.socket
            .flush()
            .await
            .map_err(|_| InternalChannelError("internal_hop_lost"))
    }
}

async fn bounded<T>(
    timeout_ms: Option<u64>,
    run: impl std::future::Future<Output = Result<T, InternalChannelError>>,
) -> Result<T, InternalChannelError> {
    match timeout_ms {
        Some(ms) => tokio::time::timeout(std::time::Duration::from_millis(ms), run)
            .await
            .map_err(|_| InternalChannelError("internal_auth_timeout"))?,
        None => run.await,
    }
}

/// senpi `connectInternalConnection`. `timeout_ms` bounds the whole client handshake on a monotonic timer; `None`
/// has no deadline. On failure the socket is dropped (closed).
pub async fn connect_internal_connection<S: AsyncRead + AsyncWrite + Unpin>(
    socket: S,
    key: &[u8],
    timeout_ms: Option<u64>,
) -> Result<InternalRecordStream<S>, InternalChannelError> {
    let mut hop = Handshake {
        socket,
        buffer: Vec::new(),
        read_bytes: 0,
        max_bytes: MAX_RPC_LINE_CHARACTERS,
    };
    let (client_nonce, server_nonce) = bounded(timeout_ms, async {
        let client_nonce = random_nonce()?;
        hop.send(json!({"id":"internal-protocol","type":"get_protocol_info","observe":true,"auth":{"client_nonce":client_nonce}}))
            .await?;
        let response = hop.read().await?;
        // A reply without a server proof is a host that cannot prove the key (or speaks another protocol).
        let unproven = InternalChannelError("host_identity_unproven");
        let auth = response
            .get("data")
            .and_then(Value::as_object)
            .and_then(|data| data.get("auth"))
            .and_then(Value::as_object)
            .ok_or_else(|| unproven.clone())?;
        let server_nonce = auth
            .get("server_nonce")
            .and_then(Value::as_str)
            .filter(|nonce| is_rpc_auth_hex(nonce))
            .ok_or(InternalChannelError("auth_failed"))?
            .to_owned();
        if response.get("id") != Some(&json!("internal-protocol"))
            || response.get("success") != Some(&Value::Bool(true))
            || auth.get("launch_id") != Some(&json!(INTERNAL_LAUNCH_ID))
            || !rpc_proof_matches(
                &proof(key, "server", &client_nonce, &server_nonce),
                auth.get("server_proof"),
            )
        {
            return Err(unproven);
        }
        hop.send(json!({"id":"internal-auth","type":"authenticate","client_proof":proof(key,"client",&server_nonce,&client_nonce)}))
            .await?;
        let result = hop.read().await?;
        if result.get("id") != Some(&json!("internal-auth"))
            || result.get("command") != Some(&json!("authenticate"))
            || result.get("success") != Some(&Value::Bool(true))
        {
            return Err(InternalChannelError("auth_failed"));
        }
        Ok((client_nonce, server_nonce))
    })
    .await?;
    seal(hop, key, &client_nonce, &server_nonce, InternalRole::Client)
}

fn seal<S>(
    hop: Handshake<S>,
    key: &[u8],
    client_nonce: &str,
    server_nonce: &str,
    role: InternalRole,
) -> Result<InternalRecordStream<S>, InternalChannelError> {
    let client = hex::decode(client_nonce).map_err(|_| InternalChannelError("auth_failed"))?;
    let server = hex::decode(server_nonce).map_err(|_| InternalChannelError("auth_failed"))?;
    Ok(create_internal_record_stream(
        hop.socket, key, &client, &server, role, hop.buffer,
    ))
}

/// senpi `acceptInternalConnection`. Wire contract frame 5: any other first frame, a first frame without a valid
/// nonce, or a command before `authenticate` is answered `auth_required` naming that command, then the connection
/// closes; a wrong proof is answered `auth_failed`.
pub async fn accept_internal_connection<S: AsyncRead + AsyncWrite + Unpin>(
    socket: S,
    key: &[u8],
    protocol: Frame,
) -> Result<InternalRecordStream<S>, InternalChannelError> {
    let mut hop = Handshake {
        socket,
        buffer: Vec::new(),
        read_bytes: 0,
        max_bytes: HOST_PRE_AUTH_MAX_BYTES,
    };
    let (client_nonce, server_nonce) = bounded(Some(HOST_PRE_AUTH_TIMEOUT_MS), async {
        async fn refuse<S: AsyncRead + AsyncWrite + Unpin>(
            hop: &mut Handshake<S>,
            frame: &Frame,
        ) -> InternalChannelError {
            let mut reply = Map::new();
            if let Some(id) = frame.get("id") {
                reply.insert("id".into(), id.clone());
            }
            reply.insert("type".into(), "response".into());
            if let Some(command) = frame.get("type") {
                reply.insert("command".into(), command.clone());
            }
            reply.insert("success".into(), false.into());
            reply.insert("error".into(), "auth_required".into());
            reply.insert("errorCode".into(), "auth_required".into());
            let _ = hop.send(Value::Object(reply)).await;
            InternalChannelError("auth_required")
        }
        let first = hop.read().await?;
        let offered = first
            .get("auth")
            .and_then(Value::as_object)
            .and_then(|auth| auth.get("client_nonce"))
            .and_then(Value::as_str)
            .filter(|nonce| is_rpc_auth_hex(nonce))
            .map(str::to_owned);
        let Some(client_nonce) = offered.filter(|_| first.get("type") == Some(&json!("get_protocol_info")))
        else {
            return Err(refuse(&mut hop, &first).await);
        };
        let server_nonce = random_nonce()?;
        let mut data = protocol.clone();
        let mut capabilities = data
            .get("capabilities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        capabilities.push("socket_auth".into());
        data.insert("capabilities".into(), Value::Array(capabilities));
        data.insert(
            "auth".into(),
            json!({"launch_id":INTERNAL_LAUNCH_ID,"server_nonce":server_nonce,"server_proof":proof(key,"server",&client_nonce,&server_nonce)}),
        );
        let mut reply = Map::new();
        if let Some(id) = first.get("id") {
            reply.insert("id".into(), id.clone());
        }
        reply.insert("type".into(), "response".into());
        reply.insert("command".into(), "get_protocol_info".into());
        reply.insert("success".into(), true.into());
        reply.insert("data".into(), Value::Object(data));
        hop.send(Value::Object(reply)).await?;
        let frame = hop.read().await?;
        if frame.get("type") != Some(&json!("authenticate")) {
            return Err(refuse(&mut hop, &frame).await);
        }
        let valid = rpc_proof_matches(
            &proof(key, "client", &server_nonce, &client_nonce),
            frame.get("client_proof"),
        );
        let mut reply = Map::new();
        if let Some(id) = frame.get("id") {
            reply.insert("id".into(), id.clone());
        }
        reply.insert("type".into(), "response".into());
        reply.insert("command".into(), "authenticate".into());
        reply.insert("success".into(), valid.into());
        if !valid {
            reply.insert("error".into(), "auth_failed".into());
            reply.insert("errorCode".into(), "auth_failed".into());
        }
        hop.send(Value::Object(reply)).await?;
        if !valid {
            return Err(InternalChannelError("auth_failed"));
        }
        Ok((client_nonce, server_nonce))
    })
    .await?;
    seal(hop, key, &client_nonce, &server_nonce, InternalRole::Server)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    const KEY: [u8; 32] = [7; 32];

    async fn lines(stream: tokio::net::UnixStream) -> Vec<Value> {
        let mut reader = BufReader::new(stream).lines();
        let mut out = Vec::new();
        while let Ok(Some(line)) = reader.next_line().await {
            out.push(serde_json::from_str(&line).expect("json"));
        }
        out
    }

    #[tokio::test]
    async fn uses_the_canonical_newline_hex_hmac_frames_then_seals_both_directions() {
        let (client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server =
            tokio::spawn(async move { accept_internal_connection(host, &KEY, Map::new()).await });
        let mut client = connect_internal_connection(client, &KEY, Some(5000))
            .await
            .expect("client");
        let mut host = server.await.expect("join").expect("server");
        client.write_all(b"{\"type\":\"ping\"}\n").await.expect("w");
        client.flush().await.expect("f");
        let mut received = [0u8; 16];
        host.read_exact(&mut received).await.expect("r");
        assert_eq!(&received, b"{\"type\":\"ping\"}\n");
    }

    #[tokio::test]
    async fn rejects_a_listener_that_cannot_prove_the_internal_key() {
        let (client, host) = tokio::net::UnixStream::pair().expect("pair");
        tokio::spawn(async move { accept_internal_connection(host, &[8; 32], Map::new()).await });
        let error = connect_internal_connection(client, &KEY, Some(5000))
            .await
            .err()
            .expect("refused");
        assert_eq!(error.0, "host_identity_unproven");
    }

    async fn raw_exchange(frames: Vec<Value>) -> Vec<Value> {
        let (mut client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server =
            tokio::spawn(async move { accept_internal_connection(host, &KEY, Map::new()).await });
        for frame in frames {
            client
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .expect("w");
        }
        assert!(server.await.expect("join").is_err());
        lines(client).await
    }

    fn auth_required(id: &str, command: &str) -> Value {
        json!({"id":id,"type":"response","command":command,"success":false,"error":"auth_required","errorCode":"auth_required"})
    }

    #[tokio::test]
    async fn answers_a_first_frame_without_or_with_a_malformed_nonce_auth_required_then_closes() {
        for auth in [None, Some(json!({"client_nonce":"zz"}))] {
            let mut frame = json!({"id":"p","type":"get_protocol_info"});
            if let Some(auth) = auth {
                frame["auth"] = auth;
            }
            assert_eq!(
                raw_exchange(vec![frame]).await,
                vec![auth_required("p", "get_protocol_info")]
            );
        }
    }

    #[tokio::test]
    async fn answers_a_command_before_authenticate_auth_required_naming_it() {
        let responses = raw_exchange(vec![
            json!({"id":"p","type":"get_protocol_info","auth":{"client_nonce":"0a".repeat(32)}}),
            json!({"id":"x","type":"list_sessions"}),
        ])
        .await;
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["data"]["capabilities"], json!(["socket_auth"]));
        assert_eq!(responses[1], auth_required("x", "list_sessions"));
    }

    #[tokio::test]
    async fn keeps_a_wrong_proof_auth_failed_then_closes() {
        let responses = raw_exchange(vec![
            json!({"id":"p","type":"get_protocol_info","auth":{"client_nonce":"0a".repeat(32)}}),
            json!({"id":"a","type":"authenticate","client_proof":"00".repeat(32)}),
        ])
        .await;
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[1],
            json!({"id":"a","type":"response","command":"authenticate","success":false,"error":"auth_failed","errorCode":"auth_failed"})
        );
    }

    #[tokio::test]
    async fn the_host_drops_a_pre_auth_frame_over_its_8_kib_budget() {
        let (mut client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server =
            tokio::spawn(async move { accept_internal_connection(host, &KEY, Map::new()).await });
        let _ = client.write_all(&vec![b'x'; 9000]).await;
        assert_eq!(
            server.await.expect("join").err().expect("dropped").0,
            "internal_auth_oversized"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn completes_a_client_handshake_whose_host_answers_after_more_than_2_s() {
        let (client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server = tokio::spawn(async move {
            let (read, mut write) = host.into_split();
            let mut reader = BufReader::new(read).lines();
            let frame: Value =
                serde_json::from_str(&reader.next_line().await.expect("r").expect("line"))
                    .expect("json");
            let client_nonce = frame["auth"]["client_nonce"]
                .as_str()
                .expect("nonce")
                .to_owned();
            let nonce = "0c".repeat(32);
            tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
            let answer = json!({"id":frame["id"],"type":"response","command":"get_protocol_info","success":true,"data":{"capabilities":["socket_auth"],"auth":{"launch_id":"internal","server_nonce":nonce,"server_proof":proof(&KEY,"server",&client_nonce,&nonce)}}});
            write
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .expect("w");
            let frame: Value =
                serde_json::from_str(&reader.next_line().await.expect("r").expect("line"))
                    .expect("json");
            assert_eq!(
                frame["client_proof"],
                json!(proof(&KEY, "client", &nonce, &client_nonce))
            );
            write
                .write_all(b"{\"id\":\"internal-auth\",\"type\":\"response\",\"command\":\"authenticate\",\"success\":true}\n")
                .await
                .expect("w");
            (reader, write)
        });
        assert!(
            connect_internal_connection(client, &KEY, None)
                .await
                .is_ok()
        );
        drop(server.await.expect("join"));
    }
}
