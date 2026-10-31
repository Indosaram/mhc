use crate::host_protocol_info::HostProtocolInfo;
use serde::Serialize;
use serde_json::Value;
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct HostSessionCounts {
    pub total: usize,
    pub interactive: usize,
    pub worker: usize,
    pub retained: usize,
    pub foreign_attached: usize,
    pub foreign_retained: usize,
}
pub fn session_counts(reply: &Value) -> HostSessionCounts {
    let mut counts = HostSessionCounts::default();
    if let Some(rows) = reply.get("sessions").and_then(Value::as_array) {
        for row in rows.iter().filter(|row| row.is_object()) {
            counts.total += 1;
            if row.get("kind").and_then(Value::as_str) == Some("worker") {
                counts.worker += 1;
            } else {
                counts.interactive += 1;
            }
            if row.get("attachments").and_then(Value::as_f64).unwrap_or(0.) > 0. {
                counts.foreign_attached += 1;
            } else {
                counts.retained += 1;
                counts.foreign_retained += 1;
            }
        }
    }
    counts
}
/// The occupancy a stop decision is made on. An observing read: looking must never keep a host from idling out.
pub async fn read_session_counts(socket: &str, include_workers: bool) -> HostSessionCounts {
    read_session_counts_within(socket, include_workers, 10000).await
}
async fn read_session_counts_within(
    socket: &str,
    include_workers: bool,
    timeout_ms: u64,
) -> HostSessionCounts {
    let mut command = serde_json::json!({
        "type": "list_sessions",
        crate::host_observe_request::OBSERVE_REQUEST_FIELD: true,
    });
    if include_workers {
        command["include_workers"] = true.into();
    }
    session_counts(
        &crate::host_probe::request_on_socket(socket, &command, timeout_ms)
            .await
            .unwrap_or(Value::Null),
    )
}
pub fn host_summary(host: Option<&HostProtocolInfo>) -> Value {
    match host {
        None => Value::Null,
        Some(host) => {
            serde_json::json!({"protocolVersion":host.protocol_version,"serverVersion":host.server_version,"capabilities":host.capabilities,"instanceId":host.instance_id,"generation":host.generation,"engineVersion":host.engine_version,"launchProfileId":host.launch_profile.as_ref().map(|profile|&profile.profile_id)})
        }
    }
}
pub async fn read_host_status(
    socket: &str,
    agent_dir: &std::path::Path,
    include_workers: bool,
) -> std::io::Result<Value> {
    read_host_status_with(socket, agent_dir, include_workers, true, 10000).await
}
/// The single-socket report. `prune: false` is how `status --all` reads: looking removes nothing (senpi `probeHostStatus`).
pub async fn read_host_status_with(
    socket: &str,
    agent_dir: &std::path::Path,
    include_workers: bool,
    prune: bool,
    timeout_ms: u64,
) -> std::io::Result<Value> {
    // The host's key record lives under its own agent dir; a looker that names it proves the key from there.
    crate::host_probe::with_rpc_key_root(
        crate::host_probe::rpc_key_root(None, Some(agent_dir)),
        read_host_status_in_key_root(socket, agent_dir, include_workers, prune, timeout_ms),
    )
    .await
}
async fn read_host_status_in_key_root(
    socket: &str,
    agent_dir: &std::path::Path,
    include_workers: bool,
    prune: bool,
    timeout_ms: u64,
) -> std::io::Result<Value> {
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    // Both reads observe: looking at a host must never be what keeps it from idling out.
    let answered = crate::host_probe::request_on_socket(
        socket,
        &serde_json::json!({
            "type": "get_protocol_info",
            crate::host_observe_request::OBSERVE_REQUEST_FIELD: true,
        }),
        timeout_ms,
    )
    .await;
    let host = answered
        .as_ref()
        .and_then(crate::host_protocol_info::parse_host_protocol_info);
    if prune {
        crate::host_generations::prune_dead_generations(&paths)?;
    }
    // A lost pointer is given back to the one proven live generation (senpi#1911); `prune: false` only looks.
    let pointer = crate::host_pointer_election::elect_pointer(&paths, socket, prune);
    // `prune: false` removes nothing, so what ended is shown as `alive: false` rows instead.
    let generations = crate::host_generations::read_generation_rows_with(
        &paths,
        crate::host_generations::GenerationRowOptions {
            include_dead: !prune,
        },
    )?;
    let current = generations.iter().find(|row| row.current && row.alive);
    let metrics = current
        .map(|row| {
            crate::host_process_metrics::read_host_process_metrics(
                row.pid,
                if cfg!(windows) {
                    "win32"
                } else {
                    std::env::consts::OS
                },
            )
        })
        .unwrap_or_default();
    let registered = crate::host_daemon_registration::read_host_registration(&paths)?;
    let process_start_time = process_start_time_for(registered.as_ref(), current);
    // A socket that did not answer who it is (or is stopping) will not answer what it holds: asking again would
    // only spend a second budget on a hung endpoint.
    let silent_or_stopping = host.as_ref().is_none_or(|host| {
        host.lifecycle == Some(crate::host_protocol_info::HostLifecycle::Stopping)
    });
    let sessions = if silent_or_stopping {
        HostSessionCounts::default()
    } else {
        read_session_counts_within(socket, include_workers, timeout_ms).await
    };
    let env_keys = crate::host_daemon_env::read_daemon_env_keys(&paths)?;
    let identity = answered.as_ref().zip(host.as_ref());
    let mut report = serde_json::json!({"reachable":host.is_some(),"socket":socket,"pid":current.map(|row|row.pid),"processStartTime":process_start_time,"instanceId":host.as_ref().and_then(|host|host.instance_id.as_deref()).or_else(||current.map(|row|row.instance_id.as_str())),"generation":host.as_ref().and_then(|host|host.generation).or_else(||current.map(|row|row.generation)),"engineVersion":host.as_ref().and_then(|host|host.engine_version.as_deref()).or_else(||current.and_then(|row|row.engine_version.as_deref())),"capabilities":host.as_ref().map(|host|host.capabilities.as_slice()).unwrap_or(&[]),"launchProfile":host.as_ref().and_then(|host|host.launch_profile.as_ref()),"sessions":sessions,"zombies":metrics.zombies,"rss_mb":metrics.rss_mb,"host_rss_mb":metrics.host_rss_mb,"host_footprint_mb":metrics.host_footprint_mb,"open_fds":metrics.open_fds,"env_keys":env_keys,"generations":generations,"pointer":pointer});
    report["lifecycle"] = host_lifecycle(host.as_ref()).into();
    report["runtimeBuildId"] = identity
        .and_then(|(data, _)| {
            crate::runtime_build_id::parse_runtime_build_id(&data["runtimeBuildId"])
        })
        .into();
    report["handover"] = host
        .as_ref()
        .and_then(|host| host.handover.clone())
        .unwrap_or(Value::Null);
    report["memory_pressure"] = host.as_ref().and_then(|host| host.memory_pressure).into();
    Ok(report)
}
/// The answering host's lifecycle (`ready` / `stopping`); `None` when nothing answers or it predates the field.
fn host_lifecycle(host: Option<&HostProtocolInfo>) -> Option<&'static str> {
    host?.lifecycle.map(|lifecycle| match lifecycle {
        crate::host_protocol_info::HostLifecycle::Ready => "ready",
        crate::host_protocol_info::HostLifecycle::Stopping => "stopping",
    })
}
/// The registered host's start time, only when it is the very generation `current` names (same instance and pid).
pub fn process_start_time_for(
    registered: Option<&crate::host_daemon_registration::RegisteredHost>,
    current: Option<&crate::host_generations::HostGenerationRow>,
) -> Option<String> {
    let (registered, current) = (registered?, current?);
    (registered.instance_id == current.instance_id && registered.pid == current.pid)
        .then(|| registered.process_start_time.clone())
        .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_malformed_rows_and_counts_foreign_occupancy() {
        let counts = session_counts(
            &serde_json::json!({"sessions":[null,{}, {"kind":"worker","attachments":2},{"kind":"worker","attachments":-1},[]]}),
        );
        assert_eq!(
            counts,
            HostSessionCounts {
                total: 3,
                interactive: 1,
                worker: 2,
                retained: 2,
                foreign_attached: 1,
                foreign_retained: 2
            }
        );
        assert_eq!(session_counts(&Value::Null), HostSessionCounts::default());
        assert_eq!(host_summary(None), Value::Null);
    }
    /// A fake host answering every request line with `info` for `get_protocol_info` and one session otherwise; it
    /// returns every request it received once the reader is done with it.
    #[cfg(unix)]
    async fn status_of_fake_host(info: Value) -> (Value, Vec<Value>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let serve = tokio::spawn(async move {
            let mut seen = Vec::new();
            loop {
                let (socket, _) = tokio::select! {
                    accepted = listener.accept() => accepted.unwrap(),
                    _ = &mut stopped => return seen,
                };
                let (read, mut write) = socket.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                while let Some(line) = lines.next_line().await.unwrap() {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let data = if request["type"] == "get_protocol_info" {
                        info.clone()
                    } else {
                        serde_json::json!({"sessions": [{"kind": "interactive"}]})
                    };
                    seen.push(request.clone());
                    let response =
                        serde_json::json!({"id": request["id"], "success": true, "data": data});
                    write
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let report = read_host_status(path.to_str().unwrap(), temp.path(), false)
            .await
            .unwrap();
        stop.send(()).unwrap();
        (report, serve.await.unwrap())
    }
    // senpi host-status.ts: both reads observe, and a stopping host is not asked what it holds.
    #[cfg(unix)]
    #[tokio::test]
    async fn status_reads_observe_and_skip_the_listing_of_a_stopping_host() {
        let info = serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[],"instanceId":"i"});
        let (report, seen) = status_of_fake_host(info.clone()).await;
        assert_eq!(report["sessions"]["total"], 1);
        assert!(
            seen.iter()
                .any(|request| request["type"] == "list_sessions")
        );
        assert!(seen.iter().all(|request| request["observe"] == true));
        let mut stopping = info;
        stopping["lifecycle"] = "stopping".into();
        let (report, seen) = status_of_fake_host(stopping).await;
        assert_eq!(report["lifecycle"], "stopping");
        assert_eq!(report["sessions"]["total"], 0);
        assert!(
            seen.iter()
                .all(|request| request["type"] == "get_protocol_info")
        );
        assert!(seen.iter().all(|request| request["observe"] == true));
    }
    #[tokio::test]
    async fn absent_endpoint_has_complete_nullable_status() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("absent.sock");
        let report = read_host_status(socket.to_str().unwrap(), temp.path(), true)
            .await
            .unwrap();
        assert_eq!(report["reachable"], false);
        assert!(report["lifecycle"].is_null());
        assert!(report["runtimeBuildId"].is_null());
        assert!(report["handover"].is_null());
        assert!(report["memory_pressure"].is_null());
        assert!(report["pid"].is_null());
        assert!(report["processStartTime"].is_null());
        assert!(report["host_footprint_mb"].is_null());
        assert_eq!(report["sessions"]["total"], 0);
        assert_eq!(report["env_keys"], serde_json::json!([]));
        assert_eq!(
            report["pointer"],
            serde_json::json!({"state":"missing","instanceId":null,"candidates":[]})
        );
    }
}
