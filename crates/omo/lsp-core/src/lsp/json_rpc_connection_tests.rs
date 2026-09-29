use super::*;
use crate::abort::AbortController;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tokio::io::DuplexStream;

async fn read_message(stream: &mut DuplexStream, buffer: &mut Vec<u8>) -> Value {
    loop {
        if let Some(header_end) = find_subsequence(buffer, HEADER_SEPARATOR) {
            let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
            let length = parse_content_length(&headers).expect("content-length");
            let body_start = header_end + HEADER_SEPARATOR.len();
            if buffer.len() >= body_start + length {
                let body: Value =
                    serde_json::from_slice(&buffer[body_start..body_start + length]).expect("json");
                buffer.drain(..body_start + length);
                return body;
            }
        }
        let mut chunk = [0_u8; 4096];
        let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut chunk))
            .await
            .expect("waited for JSON-RPC message")
            .expect("read");
        assert!(read > 0, "stream closed before a full message arrived");
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

#[tokio::test]
async fn given_active_request_when_signal_aborts_then_cancel_uses_same_id_and_late_responses_are_ignored()
 {
    let (mut server_to_client, client_reader) = tokio::io::duplex(64 * 1024);
    let (client_writer, mut client_to_server) = tokio::io::duplex(64 * 1024);
    let connection = JsonRpcConnection::new(client_reader, client_writer);
    connection.listen();
    let controller = AbortController::new();
    let signal = controller.signal();

    let request = {
        let connection = connection.clone();
        tokio::spawn(async move {
            connection
                .send_request(
                    "textDocument/diagnostic",
                    Some(json!({ "textDocument": { "uri": "file:///a.ts" } })),
                    Some(&signal),
                )
                .await
        })
    };
    let mut buffer = Vec::new();
    let first = read_message(&mut client_to_server, &mut buffer).await;
    let request_id = first["id"].clone();
    assert!(request_id.is_number() || request_id.is_string());

    controller.abort_with(LspError::other("caller cancelled"));
    let second = read_message(&mut client_to_server, &mut buffer).await;
    let rejected = request.await.expect("join");
    assert_eq!(rejected, Err(LspError::other("caller cancelled")));
    assert_eq!(connection.pending_request_count(), 0);
    assert_eq!(second["method"], json!("$/cancelRequest"));
    assert_eq!(second["params"]["id"], request_id);

    server_to_client
        .write_all(&encode(
            &json!({ "jsonrpc": "2.0", "id": request_id, "result": { "items": [] } }),
        ))
        .await
        .expect("write late response");
    assert_eq!(connection.pending_request_count(), 0);
}

#[tokio::test]
async fn unknown_server_request_gets_method_not_found_and_error_responses_carry_the_code() {
    let (mut server_to_client, client_reader) = tokio::io::duplex(64 * 1024);
    let (client_writer, mut client_to_server) = tokio::io::duplex(64 * 1024);
    let connection = JsonRpcConnection::new(client_reader, client_writer);
    connection.listen();
    let mut buffer = Vec::new();

    server_to_client
        .write_all(&encode(
            &json!({ "jsonrpc": "2.0", "id": 7, "method": "x/unknown" }),
        ))
        .await
        .expect("write");
    let reply = read_message(&mut client_to_server, &mut buffer).await;
    assert_eq!(
        reply,
        json!({ "jsonrpc": "2.0", "id": 7, "error": { "code": -32601, "message": "Method not found: x/unknown" } })
    );

    let request = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.send_request("x/fails", None, None).await })
    };
    let sent = read_message(&mut client_to_server, &mut buffer).await;
    assert_eq!(
        sent,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "x/fails" })
    );
    server_to_client
        .write_all(&encode(
            &json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "Method not found" } }),
        ))
        .await
        .expect("write");
    let error = request.await.expect("join").expect_err("error response");
    assert_eq!(error.name(), "JsonRpcError(-32601)");
    assert_eq!(error.to_string(), "Method not found");
}

#[tokio::test]
async fn dispose_rejects_pending_requests() {
    let (_server_to_client, client_reader) = tokio::io::duplex(64 * 1024);
    let (client_writer, mut client_to_server) = tokio::io::duplex(64 * 1024);
    let connection = JsonRpcConnection::new(client_reader, client_writer);
    connection.listen();
    let request = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.send_request("x/pending", None, None).await })
    };
    let mut buffer = Vec::new();
    read_message(&mut client_to_server, &mut buffer).await;

    connection.dispose();

    assert_eq!(
        request.await.expect("join"),
        Err(LspError::other("JSON-RPC connection disposed"))
    );
    assert_eq!(
        connection.send_request("x/after", None, None).await,
        Err(LspError::other("JSON-RPC connection is disposed"))
    );
}
