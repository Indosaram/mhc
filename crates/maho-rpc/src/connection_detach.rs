//! Answer `detach_connection` only after the asking connection's attachments are released
//! (senpi `rpc/connection-detach.ts`, senpi#3238). `deferred` names the sessions still counting it: one
//! whose running turn this release would end, or every session while the host drains.
use serde_json::{Value, json};
use std::future::Future;

pub async fn detach_connection<F, Fut>(
    id: Option<&Value>,
    connection_id: Option<&str>,
    release: F,
) -> Value
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Vec<String>>,
{
    let id = id.cloned().unwrap_or(Value::Null);
    let Some(connection_id) = connection_id else {
        return json!({
            "id": id,
            "type": "response",
            "command": "detach_connection",
            "success": false,
            "error": "detach_connection needs a socket connection",
        });
    };
    let deferred = release(connection_id.to_owned()).await;
    json!({
        "id": id,
        "type": "response",
        "command": "detach_connection",
        "success": true,
        "data": { "deferred": deferred },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_after_release_with_deferred_sessions() {
        let response = detach_connection(Some(&json!("r1")), Some("c1"), |connection| async move {
            assert_eq!(connection, "c1");
            vec!["s1".to_owned()]
        })
        .await;
        assert_eq!(
            response,
            json!({"id":"r1","type":"response","command":"detach_connection","success":true,"data":{"deferred":["s1"]}})
        );
    }

    #[tokio::test]
    async fn refuses_without_a_socket_connection() {
        let response = detach_connection(None, None, |_| async { Vec::new() }).await;
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"],
            "detach_connection needs a socket connection"
        );
    }
}
