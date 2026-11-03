//! WHETHER an endpoint can be talked to right now (senpi `rpc/host-endpoint-liveness.ts`).
//!
//! `routable` (answered with a recorded instance), `dead` (nothing answered and no recorded generation
//! still runs) or `live_unresponsive` (everything else). "dead" is judged exactly as `host gc` judges its
//! first condition. Reads only: removes nothing and signals nothing.
//!
//! The verdict table and budgets live in `host_endpoints` (`judge_endpoint_liveness`,
//! `endpoint_probe_timeout_ms`, `TUI_PROBE_TIMEOUT_MS`); this module adds the probing entry point.
use crate::host_daemon_paths::host_daemon_directory_paths;
use crate::host_endpoints::HostEndpointEntry;
pub use crate::host_endpoints::{
    EndpointLiveness, TUI_PROBE_TIMEOUT_MS, endpoint_probe_timeout_ms, judge_endpoint_liveness,
};
use crate::host_observe_request::OBSERVE_REQUEST_FIELD;
use crate::host_protocol_info::parse_host_protocol_info;

/// Probes the endpoint once with an observing read, under its kind's budget, and judges it.
pub async fn classify_endpoint_liveness(
    entry: &HostEndpointEntry,
    timeout_ms: Option<u64>,
) -> EndpointLiveness {
    let answered = match entry.socket.as_deref() {
        None => None,
        Some(socket) => {
            let mut request = serde_json::json!({"type": "get_protocol_info"});
            request[OBSERVE_REQUEST_FIELD] = serde_json::Value::Bool(true);
            crate::host_probe::request_on_socket(
                socket,
                &request,
                endpoint_probe_timeout_ms(entry.endpoint_kind, timeout_ms),
            )
            .await
            .and_then(|answer| parse_host_protocol_info(&answer))
            .map(|info| info.instance_id)
        }
    };
    judge_endpoint_liveness(
        &host_daemon_directory_paths(&entry.dir),
        answered.as_ref().map(Option::as_deref),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_endpoints::EndpointKind;

    #[tokio::test]
    async fn an_entry_with_no_socket_and_no_generation_is_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = HostEndpointEntry {
            socket: None,
            dir: dir.path().join("0123456789abcdef"),
            identity: "unknown",
            endpoint_kind: EndpointKind::RpcHost,
        };
        assert_eq!(
            classify_endpoint_liveness(&entry, None).await,
            EndpointLiveness::Dead
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_silent_socket_with_no_generation_is_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("h.sock");
        let entry = HostEndpointEntry {
            socket: Some(socket.to_string_lossy().into_owned()),
            dir: dir.path().join("0123456789abcdef"),
            identity: "endpoint",
            endpoint_kind: EndpointKind::Tui,
        };
        assert_eq!(
            classify_endpoint_liveness(&entry, Some(200)).await,
            EndpointLiveness::Dead
        );
    }
}
