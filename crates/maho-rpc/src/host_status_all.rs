//! `host status --all`: one status row per endpoint an agent directory holds state for (senpi `host-status-all.ts`).
//!
//! Enumeration is the daemon directory (`list_host_endpoints`) and every row is the single-socket report read
//! with `prune: false`, so this answer removes NOTHING. A directory that names no socket still gets a row
//! (`socket: null`) built from the directory alone. Every row carries `endpoint_kind` and the liveness verdict as
//! `alive` plus `reason` when it is not routable; a `tui` row also carries `owner`.
use crate::host_endpoints::{
    EndpointKind, HostEndpointEntry, endpoint_probe_timeout_ms, judge_endpoint_liveness,
    list_host_endpoints,
};
use serde_json::{Value, json};
use std::path::Path;

/// Endpoints read at once, so hundreds of hung hosts never exhaust descriptors.
pub const STATUS_ALL_MAX_IN_FLIGHT: usize = 64;

pub async fn read_all_host_status(
    agent_dir: &Path,
    include_workers: bool,
    timeout_ms: Option<u64>,
) -> std::io::Result<Vec<Value>> {
    let endpoints = list_host_endpoints(agent_dir);
    let mut rows = Vec::with_capacity(endpoints.len());
    for batch in endpoints.chunks(STATUS_ALL_MAX_IN_FLIGHT) {
        let reads = futures::future::join_all(
            batch
                .iter()
                .map(|endpoint| endpoint_status(endpoint, agent_dir, include_workers, timeout_ms)),
        )
        .await;
        for row in reads {
            rows.push(row?);
        }
    }
    Ok(rows)
}
async fn endpoint_status(
    endpoint: &HostEndpointEntry,
    agent_dir: &Path,
    include_workers: bool,
    timeout_ms: Option<u64>,
) -> std::io::Result<Value> {
    let paths = crate::host_daemon_paths::host_daemon_directory_paths(&endpoint.dir);
    let (mut report, answered) = match &endpoint.socket {
        Some(socket) => {
            let report = crate::host_status::read_host_status_with(
                socket,
                agent_dir,
                include_workers,
                false,
                endpoint_probe_timeout_ms(endpoint.endpoint_kind, timeout_ms),
            )
            .await?;
            let answered = report["reachable"] == true;
            let instance = report["instanceId"].as_str().map(str::to_owned);
            (report, answered.then_some(instance))
        }
        None => {
            let generations = crate::host_generations::read_generation_rows(&paths)?;
            let claims_live =
                crate::host_reservations::read_session_path_claims(&paths.reservations_dir).len();
            (
                json!({"reachable":false,"socket":null,"pid":null,"processStartTime":null,"instanceId":null,"generation":null,"engineVersion":null,"capabilities":[],"launchProfile":null,"sessions":{"total":0,"interactive":0,"worker":0,"retained":0,"foreign_attached":0,"foreign_retained":0},"zombies":null,"rss_mb":null,"host_footprint_mb":null,"open_fds":null,"env_keys":[],"generations":generations,"claims_live":claims_live}),
                None,
            )
        }
    };
    let verdict = judge_endpoint_liveness(&paths, answered.as_ref().map(Option::as_deref));
    let tui = endpoint.endpoint_kind == EndpointKind::Tui;
    let owner = if tui {
        json!({"pid":report["generations"].as_array().and_then(|rows|rows.iter().find(|row|row["current"]==true)).and_then(|row|row["pid"].as_u64()),"cwd":null,"session":null})
    } else {
        Value::Null
    };
    report["dir"] = json!(endpoint.dir);
    report["identity"] = endpoint.identity.into();
    report["endpoint_kind"] = endpoint.endpoint_kind.as_str().into();
    report["alive"] = (verdict == crate::host_endpoints::EndpointLiveness::Routable).into();
    report["reason"] = if verdict == crate::host_endpoints::EndpointLiveness::Routable {
        Value::Null
    } else {
        verdict.as_str().into()
    };
    report["owner"] = owner;
    // The engine-staleness judge is not ported, so no row can claim a stop candidate.
    report["stale_old_engine"] = false.into();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn an_absent_host_row_is_dead_and_a_nameless_directory_still_gets_a_row() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            read_all_host_status(temp.path(), false, None)
                .await
                .unwrap()
                .is_empty()
        );
        let socket = temp.path().join("gone.sock").to_string_lossy().into_owned();
        let paths = crate::host_daemon_paths::create_host_daemon_paths(&socket, temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        std::fs::write(&paths.endpoint_file, json!({"socket":socket}).to_string()).unwrap();
        std::fs::create_dir_all(temp.path().join("rpc-host-daemon").join("0123456789abcdef"))
            .unwrap();
        let rows = read_all_host_status(temp.path(), false, Some(200))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let named = rows
            .iter()
            .find(|row| row["socket"] == json!(socket))
            .unwrap();
        assert_eq!(
            (
                named["alive"].clone(),
                named["reason"].clone(),
                named["endpoint_kind"].clone()
            ),
            (json!(false), json!("dead"), json!("rpc_host"))
        );
        let nameless = rows.iter().find(|row| row["socket"].is_null()).unwrap();
        assert_eq!(
            (nameless["identity"].clone(), nameless["reachable"].clone()),
            (json!("unknown"), json!(false))
        );
        assert!(paths.dir.exists(), "status --all must remove nothing");
    }
}
