use maho_ext_api::{ExtensionFailure, ProviderModelConfig};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
pub const LIVE_MODELS_URL: &str = "https://api.z.ai/api/anthropic/v1/models";
pub const LIVE_MODELS_MAX_BYTES: usize = 1_048_576;
fn safe_token(value: &str) -> bool { !value.trim().is_empty() && value.encode_utf16().count() <= 200 && !value.chars().any(|character| character <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&character)) }
pub fn parse_live(payload: &Value) -> Vec<ProviderModelConfig> {
    let Some(entries) = payload.as_array().or_else(|| payload.get("data").and_then(Value::as_array)) else { return Vec::new(); };
    entries.iter().filter_map(|entry| {
        let id = entry.get("id").and_then(Value::as_str).filter(|id| safe_token(id))?;
        let name = entry.get("display_name").and_then(Value::as_str).filter(|name| safe_token(name)).unwrap_or(id);
        Some(crate::models::model_config(id.into(), name.into()))
    }).collect()
}
pub async fn fetch_live(client: &reqwest::Client, url: &str, key: &str, headers: &BTreeMap<String, String>, signal: &AbortSignal) -> Result<Vec<ProviderModelConfig>, ExtensionFailure> {
    let operation = async {
        let mut request = client.get(url).header("Accept", "application/json").bearer_auth(key).timeout(Duration::from_secs(30));
        for (key, value) in headers { request = request.header(key, value); }
        let mut response = request.send().await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if !response.status().is_success() { return Err(ExtensionFailure::new(format!("live models request failed: {}", response.status().as_u16()))); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| ExtensionFailure::new(error.to_string()))? {
            if bytes.len().saturating_add(chunk.len()) > LIVE_MODELS_MAX_BYTES { return Err(ExtensionFailure::new(format!("live models response exceeded {LIVE_MODELS_MAX_BYTES} bytes"))); }
            bytes.extend_from_slice(&chunk);
        }
        let payload = serde_json::from_str(&String::from_utf8_lossy(&bytes)).map_err(|_| ExtensionFailure::new("live models response was not valid JSON"))?;
        Ok(parse_live(&payload))
    };
    tokio::select! { biased; _ = signal.cancelled() => Err(ExtensionFailure::new("live models request cancelled")), result = operation => result }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn accepts_bare_array() { assert_eq!(parse_live(&json!([{"id":"m"}]))[0].id, "m"); }
    #[test] fn accepts_envelope() { assert_eq!(parse_live(&json!({"data":[{"id":"m","display_name":"Model"}]}))[0].name, "Model"); }
    #[test] fn rejects_control_tokens() { for id in ["", " ", "a\n", "a\u{85}"] { assert!(parse_live(&json!([{"id":id}])).is_empty()); } }
    #[test] fn rejects_long_utf16_token() { assert!(parse_live(&json!([{"id":"😀".repeat(101)}])).is_empty()); }
    #[test] fn defaults_invalid_display_name() { assert_eq!(parse_live(&json!([{"id":"m","display_name":"\n"}]))[0].name, "m"); }
    #[test] fn preserves_duplicates() { assert_eq!(parse_live(&json!([{"id":"m"},{"id":"m"}])).len(), 2); }
    async fn fixture(status: u16, body: Vec<u8>) -> Result<Vec<ProviderModelConfig>, ExtensionFailure> {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap();
        let url = format!("http://{}/models",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut stream,_) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with("GET /models "));
            let mut headers = String::new();
            loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } headers.push_str(&line.to_lowercase()); }
            assert!(headers.contains("authorization: bearer fixture-key\r\n"));
            assert!(headers.contains("x-title: fixture\r\n"));
            write!(stream,"HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            let _closed_after_cap = stream.write_all(&body);
        });
        let result = fetch_live(&reqwest::Client::new(),&url,"fixture-key",&BTreeMap::from([("X-Title".into(),"fixture".into())]),&maho_ai::utils::abort::operation_signal(None)).await;
        peer.join().unwrap();
        result
    }
    #[tokio::test] async fn wire_request_sends_credentials_and_source_headers() { assert_eq!(fixture(200,br#"{"data":[{"id":"live","display_name":"Live"}]}"#.to_vec()).await.unwrap()[0].name,"Live"); }
    #[tokio::test] async fn wire_response_is_capped() { assert!(fixture(200,vec![b' ';LIVE_MODELS_MAX_BYTES+1]).await.unwrap_err().to_string().contains("exceeded")); }
    #[tokio::test] async fn wire_rejects_http_error() { assert!(fixture(401,b"{}".to_vec()).await.unwrap_err().to_string().contains("401")); }
    #[tokio::test] async fn wire_rejects_non_json() { assert!(fixture(200,b"not-json".to_vec()).await.unwrap_err().to_string().contains("JSON")); }
    #[tokio::test] async fn wire_missing_array_is_empty() { assert!(fixture(200,br#"{"data":{"id":"not-an-array"}}"#.to_vec()).await.unwrap().is_empty()); }
}
