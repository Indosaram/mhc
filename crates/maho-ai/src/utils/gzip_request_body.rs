//! Port of senpi packages/ai/src/utils/gzip-request-body.ts.

use std::collections::HashSet;
use std::io::Write;
use std::sync::{LazyLock, Mutex};

use flate2::{Compression, write::GzEncoder};
use futures::StreamExt;
use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderMap, HeaderValue};

pub const GZIP_REQUEST_BODY_MIN_CHARS: usize = 4096;
static UNCOMPRESSED_ORIGINS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Debug, thiserror::Error)]
pub enum GzipRequestError {
    #[error(transparent)]
    Compression(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("could not generate request length padding")]
    Random,
}

fn compress(body: &str) -> Result<Vec<u8>, GzipRequestError> {
    let mut random = [0_u8; 258];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| GzipRequestError::Random)?;
    let count = usize::from(u16::from_le_bytes([random[0], random[1]])) % 257;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body.as_bytes())?;
    encoder.write_all(b"\n")?;
    for byte in &random[2..2 + count] {
        encoder.write_all(if byte & 1 == 0 { b" " } else { b"\t" })?;
    }
    Ok(encoder.finish()?)
}

async fn gzip_rejection(
    mut response: reqwest::Response,
) -> Result<(bool, reqwest::Response), reqwest::Error> {
    let status = response.status().as_u16();
    if status == 415 {
        return Ok((true, response));
    }
    if status != 400 && status != 403 {
        return Ok((false, response));
    }
    let from_anthropic = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|id| id.starts_with("req_"));
    if !from_anthropic {
        return Ok((true, response));
    }
    if status != 400 {
        return Ok((false, response));
    }
    let headers = response.headers().clone();
    let version = response.version();
    let mut prefix = Vec::new();
    while prefix.len() < 8192 {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        prefix.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&prefix);
    let rejected = [
        "The request body is not valid JSON",
        "Failed to parse request: could not parse request body as JSON",
    ]
    .iter()
    .any(|message| text.contains(message));
    // Restore consumed bytes when this was an ordinary API error, not an encoding rejection.
    let mut restored = http::Response::new(reqwest::Body::wrap_stream(
        futures::stream::once(async move { Ok::<_, reqwest::Error>(bytes::Bytes::from(prefix)) })
            .chain(response.bytes_stream()),
    ));
    *restored.status_mut() = http::StatusCode::BAD_REQUEST;
    *restored.headers_mut() = headers;
    *restored.version_mut() = version;
    Ok((rejected, reqwest::Response::from(restored)))
}

pub async fn send_with_gzip_request_body(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    body: String,
    enabled: bool,
) -> Result<reqwest::Response, GzipRequestError> {
    let origin = url::Url::parse(url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| url.to_owned());
    let disabled = UNCOMPRESSED_ORIGINS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&origin);
    if !enabled || body.encode_utf16().count() < GZIP_REQUEST_BODY_MIN_CHARS || disabled {
        return Ok(client.post(url).headers(headers).body(body).send().await?);
    }
    let mut compressed_headers = headers.clone();
    compressed_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    compressed_headers.remove(CONTENT_LENGTH);
    let response = client
        .post(url)
        .headers(compressed_headers)
        .body(compress(&body)?)
        .send()
        .await?;
    let (rejected, response) = gzip_rejection(response).await?;
    if !rejected {
        return Ok(response);
    }
    UNCOMPRESSED_ORIGINS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(origin);
    drop(response);
    Ok(client.post(url).headers(headers).body(body).send().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[tokio::test]
    async fn rejected_encoding_retries_and_stays_uncompressed() -> anyhow::Result<()> {
        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured = received.clone();
        let app = axum::Router::new().route(
            "/messages",
            axum::routing::post(move |headers: HeaderMap, body: bytes::Bytes| {
                let captured = captured.clone();
                async move {
                    let compressed = headers.contains_key(CONTENT_ENCODING);
                    captured
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push((compressed, body));
                    if compressed {
                        http::StatusCode::UNSUPPORTED_MEDIA_TYPE
                    } else {
                        http::StatusCode::OK
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/messages", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let body = serde_json::json!({"input": "x".repeat(4096)}).to_string();
        let client = reqwest::Client::new();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..2 {
                let response = send_with_gzip_request_body(
                    &client,
                    &url,
                    HeaderMap::new(),
                    body.clone(),
                    true,
                )
                .await?;
                anyhow::ensure!(response.status().is_success());
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        server.abort();
        outcome??;
        let received = received.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            received.iter().map(|(gzip, _)| *gzip).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(received[1].1.as_ref(), body.as_bytes());
        assert_eq!(received[2].1.as_ref(), body.as_bytes());
        Ok(())
    }

    #[test]
    fn compressed_body_retains_json_and_only_adds_whitespace() {
        // Given a JSON request, when compressed, then decoding preserves the JSON value.
        let body = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let compressed = compress(body).expect("compression");
        let mut decoded = String::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_string(&mut decoded)
            .expect("decompression");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decoded).expect("json"),
            serde_json::from_str::<serde_json::Value>(body).expect("original json")
        );
        assert!(
            decoded
                .strip_prefix(body)
                .expect("prefix")
                .chars()
                .all(char::is_whitespace)
        );
    }
}
