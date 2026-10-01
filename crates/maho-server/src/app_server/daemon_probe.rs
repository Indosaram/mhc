use serde_json::{Value, json};
use std::path::Path;

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
