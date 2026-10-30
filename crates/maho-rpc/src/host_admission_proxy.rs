//! The supervisor's per-client byte proxy with admission (senpi `host-admission-proxy.ts`). Requests are
//! size-bounded; each direction awaits downstream backpressure. Opens remain concurrent with control commands. Stop
//! refuses every pending open, while other commands keep their wire order and child-owned semantics.
//!
//! senpi passes the public filter as `auth` (every reply through `auth.response()`, reads from `auth.input`, no
//! batched fast path). Here a public client arrives already wrapped by `rpc_public_auth::public_rpc_stream`, whose
//! reads are the released frames and whose writes pass `response()`, so the same contract holds for any `client`.
use crate::host_admission::{HostAdmission, host_stopping_response};
use crate::jsonl::{JsonlLineReader, LineRecord, MAX_RPC_LINE_CHARACTERS, serialize_json_line};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Default)]
struct Admitted {
    pending: Vec<Value>,
    refused: Vec<Value>,
    protocols: usize,
}
impl Admitted {
    fn quiet(&self) -> bool {
        self.pending.is_empty() && self.protocols == 0 && self.refused.is_empty()
    }
}

fn record(line: &[u8]) -> Option<Map<String, Value>> {
    serde_json::from_slice::<Value>(line)
        .ok()
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
}

fn line_of(value: &Value) -> Vec<u8> {
    serialize_json_line(value)
        .unwrap_or_else(|_| format!("{value}\n"))
        .into_bytes()
}

type Writer<C> = Arc<tokio::sync::Mutex<tokio::io::WriteHalf<C>>>;

async fn reply<C: AsyncWrite>(client: &Writer<C>, bytes: &[u8]) -> std::io::Result<()> {
    let mut client = client.lock().await;
    client.write_all(bytes).await?;
    client.flush().await
}

/// Only lifecycle responses are decoded; histories and deltas stay byte slices.
fn filter_replies(batch: &[u8], state: &Mutex<Admitted>, admission: &HostAdmission) -> Vec<u8> {
    let mut admitted = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if admitted.quiet() {
        return batch.to_vec();
    }
    let mut output = Vec::with_capacity(batch.len());
    for line in batch.split_inclusive(|byte| *byte == b'\n') {
        let text = String::from_utf8_lossy(line);
        let response =
            if text.contains("\"open_session\"") || text.contains("\"get_protocol_info\"") {
                record(line)
            } else {
                None
            };
        let is_response = |command: &str| {
            response.as_ref().is_some_and(|response| {
                response.get("type") == Some(&json!("response"))
                    && response.get("command") == Some(&json!(command))
            })
        };
        if is_response("open_session") {
            let id = response
                .as_ref()
                .and_then(|response| response.get("id"))
                .cloned()
                .unwrap_or(Value::Null);
            if admitted.refused.contains(&id) {
                continue;
            }
            if let Some(index) = admitted.pending.iter().position(|pending| *pending == id) {
                admitted.pending.remove(index);
            }
        }
        if is_response("get_protocol_info") {
            admitted.protocols = admitted.protocols.saturating_sub(1);
            if let Some(mut response) = response
                .clone()
                .filter(|response| response.get("success") == Some(&Value::Bool(true)))
            {
                if let Some(Value::Object(data)) = response.get("data") {
                    admission.observe(data.clone());
                }
                response.insert("data".into(), Value::Object(admission.protocol()));
                output.extend(line_of(&Value::Object(response)));
                continue;
            }
        }
        output.extend_from_slice(line);
    }
    output
}

/// senpi `proxyWithAdmission`. Returns when the internal side ends (or, without one, when the client does), or as
/// soon as either side fails outside a stop.
pub async fn proxy_with_admission<C, I>(
    client: C,
    internal: Option<I>,
    admission: Arc<HostAdmission>,
) where
    C: AsyncRead + AsyncWrite + Send + 'static,
    I: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut client_read, client_write) = tokio::io::split(client);
    let client_write: Writer<C> = Arc::new(tokio::sync::Mutex::new(client_write));
    let state = Arc::new(Mutex::new(Admitted::default()));
    let (mut internal_read, internal_write) = match internal {
        Some(internal) => {
            let (read, write) = tokio::io::split(internal);
            (Some(read), Some(write))
        }
        None => (None, None),
    };
    let has_internal = internal_write.is_some();
    let mut internal_write = internal_write;

    let requests = {
        let client = client_write.clone();
        let state = state.clone();
        let admission = admission.clone();
        async move {
            let mut lines =
                JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS).map_err(std::io::Error::other)?;
            let mut chunk = vec![0u8; 16 * 1024];
            loop {
                let count = client_read.read(&mut chunk).await?;
                let records = if count == 0 {
                    lines.finish()
                } else {
                    lines.push(&chunk[..count])
                };
                for entry in records {
                    let line = match entry {
                        LineRecord::Oversized => {
                            reply(&client, &line_of(&json!({"type":"response","command":"parse","success":false,"error":format!("RPC input line exceeds {MAX_RPC_LINE_CHARACTERS} characters.")}))).await?;
                            continue;
                        }
                        LineRecord::Line(line) => line,
                    };
                    let request = record(line.as_bytes());
                    let kind = request.as_ref().and_then(|request| request.get("type"));
                    let id = request.as_ref().and_then(|request| request.get("id"));
                    if admission.stopping() && kind == Some(&json!("get_protocol_info")) {
                        let mut response = Map::new();
                        response.insert("id".into(), id.cloned().unwrap_or(Value::Null));
                        response.insert("type".into(), "response".into());
                        response.insert("command".into(), "get_protocol_info".into());
                        response.insert("success".into(), true.into());
                        response.insert("data".into(), Value::Object(admission.protocol()));
                        reply(&client, &line_of(&Value::Object(response))).await?;
                    } else if admission.stopping()
                        && (kind == Some(&json!("open_session")) || internal_write.is_none())
                    {
                        let command = kind
                            .map(|kind| {
                                kind.as_str()
                                    .map_or_else(|| kind.to_string(), str::to_owned)
                            })
                            .unwrap_or_else(|| "parse".into());
                        reply(
                            &client,
                            &line_of(&host_stopping_response(id, &command, &admission)),
                        )
                        .await?;
                    } else if let Some(internal) = internal_write.as_mut() {
                        // Record admission before writing: a child response can arrive immediately.
                        {
                            let mut admitted = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if kind == Some(&json!("open_session")) {
                                admitted.pending.push(id.cloned().unwrap_or(Value::Null));
                            }
                            if kind == Some(&json!("get_protocol_info")) {
                                admitted.protocols += 1;
                            }
                        }
                        internal.write_all(format!("{line}\n").as_bytes()).await?;
                        internal.flush().await?;
                    }
                }
                if count == 0 {
                    if let Some(internal) = internal_write.as_mut() {
                        let _ = internal.shutdown().await;
                    }
                    return Ok::<(), std::io::Error>(());
                }
            }
        }
    };

    let (refusals_done, mut refusals_wait) = tokio::sync::watch::channel(true);
    let stop_listener = {
        let client = client_write.clone();
        let state = state.clone();
        let admission = admission.clone();
        let mut stopped = admission.on_stop();
        async move {
            if stopped.wait_for(|stopped| *stopped).await.is_err() {
                return std::future::pending::<std::io::Result<()>>().await;
            }
            refusals_done.send_replace(false);
            let done = admission.track_reply();
            let ids: Vec<Value> = {
                let mut admitted = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let ids = std::mem::take(&mut admitted.pending);
                admitted.refused.extend(ids.iter().cloned());
                ids
            };
            let mut result = Ok(());
            for id in ids {
                result = reply(
                    &client,
                    &line_of(&host_stopping_response(
                        Some(&id),
                        "open_session",
                        &admission,
                    )),
                )
                .await;
                if result.is_err() {
                    break;
                }
            }
            let _ = done.send(());
            refusals_done.send_replace(true);
            result?;
            std::future::pending::<std::io::Result<()>>().await
        }
    };

    let responses = {
        let client = client_write.clone();
        let state = state.clone();
        let admission = admission.clone();
        async move {
            let Some(source) = internal_read.as_mut() else {
                return std::future::pending::<std::io::Result<()>>().await;
            };
            // Keep complete records together so local refusals cannot split a streaming delta.
            let mut fragments: Vec<u8> = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let count = source.read(&mut chunk).await?;
                if count == 0 {
                    break;
                }
                let chunk = &chunk[..count];
                let Some(newline) = chunk.iter().rposition(|byte| *byte == b'\n') else {
                    fragments.extend_from_slice(chunk);
                    continue;
                };
                fragments.extend_from_slice(&chunk[..=newline]);
                let batch = std::mem::take(&mut fragments);
                reply(&client, &filter_replies(&batch, &state, &admission)).await?;
                fragments.extend_from_slice(&chunk[newline + 1..]);
            }
            if !fragments.is_empty() {
                reply(&client, &filter_replies(&fragments, &state, &admission)).await?;
            }
            let _ = refusals_wait.wait_for(|done| *done).await;
            let _ = client.lock().await.shutdown().await;
            Ok(())
        }
    };

    tokio::pin!(requests);
    tokio::pin!(responses);
    tokio::pin!(stop_listener);
    let mut requests_done = false;
    loop {
        tokio::select! {
            result = &mut requests, if !requests_done => {
                requests_done = true;
                // The client is gone: its hop goes with it (senpi `client.once("close")`).
                if result.is_err() || !has_internal { return; }
            }
            result = &mut responses => {
                let _ = result;
                return;
            }
            result = &mut stop_listener => {
                if result.is_err() { return; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn admission() -> Arc<HostAdmission> {
        Arc::new(HostAdmission::new(
            json!({"protocolVersion":1,"serverVersion":"test","capabilities":[],"instanceId":"i"})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ))
    }

    async fn read_line(stream: &mut tokio::io::DuplexStream) -> Value {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).await.is_ok() && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        serde_json::from_slice(&line).expect("json line")
    }

    #[tokio::test]
    async fn stop_refuses_pending_opens_and_drops_their_late_child_replies() {
        let (mut client, proxied) = tokio::io::duplex(4096);
        let (internal, mut child) = tokio::io::duplex(4096);
        let admission = admission();
        let proxy = tokio::spawn(proxy_with_admission(
            proxied,
            Some(internal),
            admission.clone(),
        ));
        client
            .write_all(b"{\"id\":\"o\",\"type\":\"open_session\"}\n")
            .await
            .expect("w");
        let forwarded = read_line(&mut child).await;
        assert_eq!(forwarded["type"], "open_session");
        admission.stop();
        let refused = read_line(&mut client).await;
        assert_eq!(refused["errorCode"], "host_stopping");
        assert_eq!(refused["id"], "o");
        child
            .write_all(b"{\"id\":\"o\",\"type\":\"response\",\"command\":\"open_session\",\"success\":true}\n{\"type\":\"event\"}\n")
            .await
            .expect("w");
        assert_eq!(read_line(&mut client).await, json!({"type":"event"}));
        drop(child);
        proxy.await.expect("proxy");
    }

    #[tokio::test]
    async fn protocol_replies_carry_the_admission_lifecycle() {
        let (mut client, proxied) = tokio::io::duplex(4096);
        let (internal, mut child) = tokio::io::duplex(4096);
        let proxy = tokio::spawn(proxy_with_admission(proxied, Some(internal), admission()));
        client
            .write_all(b"{\"id\":\"p\",\"type\":\"get_protocol_info\"}\n")
            .await
            .expect("w");
        read_line(&mut child).await;
        child
            .write_all(b"{\"id\":\"p\",\"type\":\"response\",\"command\":\"get_protocol_info\",\"success\":true,\"data\":{\"instanceId\":\"child\"}}\n")
            .await
            .expect("w");
        let reply = read_line(&mut client).await;
        assert_eq!(
            reply["data"],
            json!({"instanceId":"child","lifecycle":"ready"})
        );
        drop(child);
        proxy.await.expect("proxy");
    }

    #[tokio::test]
    async fn a_stopping_proxy_without_a_hop_answers_locally() {
        let (mut client, proxied) = tokio::io::duplex(4096);
        let admission = admission();
        admission.stop();
        let proxy = tokio::spawn(proxy_with_admission::<_, tokio::io::DuplexStream>(
            proxied, None, admission,
        ));
        client
            .write_all(b"{\"id\":\"p\",\"type\":\"get_protocol_info\"}\n{\"id\":\"l\",\"type\":\"list_sessions\"}\n")
            .await
            .expect("w");
        assert_eq!(
            read_line(&mut client).await["data"]["lifecycle"],
            "stopping"
        );
        let refused = read_line(&mut client).await;
        assert_eq!(
            (refused["command"].clone(), refused["errorCode"].clone()),
            (json!("list_sessions"), json!("host_stopping"))
        );
        drop(client);
        proxy.await.expect("proxy");
    }
}
