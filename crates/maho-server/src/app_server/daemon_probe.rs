use serde_json::{Value, json};
use std::path::Path;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{client_async, tungstenite::{Message, client::IntoClientRequest}};

pub async fn probe_websocket(url: &str, token: Option<&str>, timeout_ms: u64, version: &str) -> Option<String> {
    let operation = async {
        let target = url::Url::parse(url).ok()?;
        let host = target.host_str()?;
        let port = target.port_or_known_default()?;
        let stream = tokio::net::TcpStream::connect((host, port)).await.ok()?;
        let mut request = url.into_client_request().ok()?;
        if let Some(token) = token.filter(|token| !token.is_empty()) { request.headers_mut().insert("authorization", format!("Bearer {token}").parse().ok()?); }
        let (mut socket, _) = client_async(request, stream).await.ok()?;
        socket.send(Message::Text(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"senpi_app_server_daemon","title":"senpi app-server daemon","version":version}}}).to_string().into())).await.ok()?;
        let result = socket.next().await?.ok()?;
        match result { Message::Text(text) => read_initialize_probe(&text), _ => None }
    };
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), operation).await.ok().flatten()
}
pub async fn probe_listen(token_file: &Path, listen: &Value, timeout_ms: u64, version: &str) -> Result<Option<String>, std::io::Error> {
    if listen["kind"] == "stdio" { return Ok(None); }
    let token = match tokio::fs::read_to_string(token_file).await {
        Ok(token) => Some(token.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && listen["kind"] == "ws" => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if listen["kind"] == "ws" { return Ok(probe_websocket(listen["url"].as_str().unwrap_or_default(), token.as_deref(), timeout_ms, version).await); }
    Ok(None)
}

pub fn read_initialize_probe(text: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    if parsed.get("id")?.as_f64() != Some(1.0) {
        return None;
    }
    parsed
        .get("result")?
        .get("userAgent")?
        .as_str()
        .map(str::to_owned)
}
pub fn parse_listen(value: &Value) -> Option<Value> {
    let kind = value.get("kind")?.as_str()?;
    let url = value.get("url")?.as_str()?;
    match kind {
        "stdio" if url == "stdio://" => Some(json!({"kind":"stdio","url":"stdio://"})),
        "unix" => {
            let mut result = json!({"kind":"unix","url":url});
            if let Some(path) = value.get("path").and_then(Value::as_str) {
                result["path"] = json!(path);
            }
            Some(result)
        }
        "ws" => {
            let host = value.get("host")?.as_str()?;
            let port = value.get("port")?;
            if !port.is_number() {
                return None;
            }
            Some(json!({"kind":"ws","url":url,"host":host,"port":port}))
        }
        _ => None,
    }
}
pub async fn read_settings(path: &Path) -> Result<Option<Value>, std::io::Error> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    Ok(parsed
        .get("listen")
        .and_then(parse_listen)
        .map(|listen| json!({"listen":listen})))
}
async fn remove(path: &Path) -> Result<(), std::io::Error> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
pub async fn cleanup_state(
    pid_file: &Path,
    settings_file: &Path,
    listen: &Value,
) -> Result<(), std::io::Error> {
    remove(pid_file).await?;
    remove(settings_file).await?;
    if listen.get("kind").and_then(Value::as_str) == Some("unix")
        && let Some(path) = listen
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
    {
        remove(Path::new(path)).await?;
    }
    Ok(())
}
pub fn running_output(status: &str, pid: Option<u32>, listen: &Value, version: &str) -> Value {
    match pid {
        Some(pid) => json!({"status":status,"pid":pid,"listen":listen["url"],"version":version}),
        None => running_unmanaged_output(listen, version),
    }
}
pub fn running_unmanaged_output(listen: &Value, version: &str) -> Value {
    json!({"status":"running-unmanaged","listen":listen["url"],"version":version})
}
