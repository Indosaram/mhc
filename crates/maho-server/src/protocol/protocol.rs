use serde_json::Value;

pub const PROTOCOL_VERSION: u64 = 8;

pub fn is_server_id(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && b.iter().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(c)
            }
        })
}

fn strict(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|v| {
        required.iter().all(|k| v.contains_key(*k))
            && v.keys()
                .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    })
}
fn id(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty())
}
fn server_id(v: &Value) -> bool {
    v.as_str().is_some_and(is_server_id)
}
fn error(v: &Value) -> bool {
    strict(v, &["code", "message"], &[]) && id(&v["code"]) && v["message"].is_string()
}
fn session_target(v: &Value) -> bool {
    strict(v, &["serverId", "sessionId", "attachmentId"], &[])
        && server_id(&v["serverId"])
        && id(&v["sessionId"])
        && id(&v["attachmentId"])
}
fn target(v: &Value) -> bool {
    (strict(v, &["serverId"], &[]) && server_id(&v["serverId"])) || session_target(v)
}

pub fn valid_client_message(v: &Value) -> bool {
    match v["type"].as_str() {
        Some("hello") => {
            strict(v, &["type", "version"], &[])
                && v["version"]
                    .as_f64()
                    .is_some_and(|n| n >= 0.0 && n.fract() == 0.0)
        }
        Some("request") => {
            strict(v, &["type", "id", "target", "call"], &[])
                && id(&v["id"])
                && target(&v["target"])
        }
        Some("cancel") => {
            strict(v, &["type", "id", "target"], &[]) && id(&v["id"]) && target(&v["target"])
        }
        _ => false,
    }
}

pub fn valid_server_message(v: &Value) -> bool {
    match v["type"].as_str() {
        Some("hello") => {
            strict(v, &["type", "version", "serverId"], &[])
                && v["version"].as_f64() == Some(8.0)
                && server_id(&v["serverId"])
        }
        Some("hello_error") => strict(v, &["type", "error"], &[]) && error(&v["error"]),
        Some("response") => match v["ok"].as_bool() {
            Some(true) => strict(v, &["type", "id", "ok"], &["result"]) && id(&v["id"]),
            Some(false) => {
                strict(v, &["type", "id", "ok", "error"], &[]) && id(&v["id"]) && error(&v["error"])
            }
            None => false,
        },
        Some("service_update") => {
            strict(v, &["type", "subscriptionId", "update"], &[]) && id(&v["subscriptionId"])
        }
        Some("attachment") => {
            strict(v, &["type", "attachment"], &[])
                && (v["attachment"].is_null() || session_target(&v["attachment"]))
        }
        _ => false,
    }
}
