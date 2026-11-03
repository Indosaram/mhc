//! Whether a RUNNING host is a stale older-engine leftover that a newer client may stop (senpi
//! `rpc/host-stale-engine.ts`, #3268). Every read errs toward "keep", and every probe is an observing read,
//! so judging a host never resets its idle window. The caller judges inside the endpoint's ensure lock and
//! stops through `stopHost`, which re-reads the session count before it signals.
use crate::host_daemon_paths::create_host_daemon_paths;
use crate::host_daemon_registration::{proven_owner, read_host_registration};
use crate::host_daemon_state::{parse_json, read_file_or_undefined};
use crate::host_lifecycle::resolve_host_policy;
use crate::host_observe_request::OBSERVE_REQUEST_FIELD;
use crate::host_probe::request_on_socket;
use crate::host_protocol_info::parse_host_protocol_info;
use crate::host_public_clients::count_public_clients;
use crate::host_reservations::read_session_path_claims;
use crate::host_stop_start_time::process_start_time_ms;
use maho_core::engine_build_identity::{
    EngineBuildIdentity, EngineOrdinalScheme, compare_engine_ordinal,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleEngineKeep {
    StaleUnprovenOwner,
    StaleUnreachable,
    StaleNotOlder,
    StalePersistent,
    StaleSessions,
    StaleClaims,
    StaleYoung,
    StaleClients,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleEngineVerdict {
    Stale { pid: u32 },
    Keep(StaleEngineKeep),
}

#[derive(Default)]
pub struct StaleEngineOptions {
    pub now: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
    pub grace_ms: Option<f64>,
}

pub const STALE_ENGINE_GRACE_MS: f64 = 10.0 * 60_000.0;
const PROBE_TIMEOUT_MS: u64 = 2_000;

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

/// The host's identity from its published ordinal; the epoch slot decides the scheme.
fn host_identity(text: Option<&str>, ordinal: Option<[f64; 5]>) -> Option<EngineBuildIdentity> {
    let ordinal = ordinal?.map(|part| part as i64);
    Some(EngineBuildIdentity {
        text: text.unwrap_or_default().to_owned(),
        ordinal,
        scheme: if ordinal[4] > 0 {
            EngineOrdinalScheme::Epoch
        } else {
            EngineOrdinalScheme::Nodef
        },
    })
}

pub async fn judge_stale_engine(
    socket: &str,
    agent_dir: &Path,
    client: &EngineBuildIdentity,
    options: StaleEngineOptions,
) -> StaleEngineVerdict {
    use StaleEngineKeep::*;
    let paths = create_host_daemon_paths(socket, agent_dir);
    let registration = read_host_registration(&paths).ok().flatten();
    let Some(owner) = proven_owner(registration.as_ref(), socket).cloned() else {
        return StaleEngineVerdict::Keep(StaleUnprovenOwner);
    };
    // Counted before this judge's own probes connect, so none of them can be read back as a client.
    let clients = count_public_clients(owner.pid, socket).await;
    let mut request = json!({"type": "get_protocol_info"});
    request[OBSERVE_REQUEST_FIELD] = Value::Bool(true);
    let Some(info) = request_on_socket(socket, &request, PROBE_TIMEOUT_MS)
        .await
        .and_then(|answer| parse_host_protocol_info(&answer))
    else {
        return StaleEngineVerdict::Keep(StaleUnreachable);
    };
    let older = host_identity(info.engine_version.as_deref(), info.engine_ordinal)
        .is_some_and(|host| compare_engine_ordinal(&host, client) == -1);
    if !older {
        return StaleEngineVerdict::Keep(StaleNotOlder);
    }
    let settings = parse_json(
        read_file_or_undefined(&paths.settings_file)
            .ok()
            .flatten()
            .as_deref(),
    )
    .map_or(Value::Null, Value::Object);
    let policy = resolve_host_policy(&settings, &HashMap::new());
    if policy.cold_start == "persistent" {
        return StaleEngineVerdict::Keep(StalePersistent);
    }
    if observed_session_count(socket).await != Some(0) {
        return StaleEngineVerdict::Keep(StaleSessions);
    }
    if !read_session_path_claims(&paths.reservations_dir).is_empty() {
        return StaleEngineVerdict::Keep(StaleClaims);
    }
    let started_at = owner
        .process_start_time
        .as_deref()
        .and_then(process_start_time_ms);
    let now = options.now.as_ref().map_or_else(now_ms, |now| now());
    let grace = options.grace_ms.unwrap_or(STALE_ENGINE_GRACE_MS);
    if started_at.is_none_or(|started_at| now - started_at < policy.idle_exit_ms + grace) {
        return StaleEngineVerdict::Keep(StaleYoung);
    }
    if clients != Some(0) {
        return StaleEngineVerdict::Keep(StaleClients);
    }
    StaleEngineVerdict::Stale { pid: owner.pid }
}

async fn observed_session_count(socket: &str) -> Option<usize> {
    let mut request = json!({"type": "list_sessions", "include_workers": true});
    request[OBSERVE_REQUEST_FIELD] = Value::Bool(true);
    request_on_socket(socket, &request, PROBE_TIMEOUT_MS)
        .await?
        .get("sessions")?
        .as_array()
        .map(Vec::len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unregistered_endpoint_is_kept_as_unproven() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("host.sock").to_string_lossy().into_owned();
        let client = EngineBuildIdentity {
            text: "2026.1.1".into(),
            ordinal: [2026, 1, 1, 0, 0],
            scheme: EngineOrdinalScheme::Nodef,
        };
        let verdict =
            judge_stale_engine(&socket, dir.path(), &client, StaleEngineOptions::default()).await;
        assert_eq!(
            verdict,
            StaleEngineVerdict::Keep(StaleEngineKeep::StaleUnprovenOwner)
        );
    }

    #[test]
    fn the_epoch_slot_picks_the_host_scheme() {
        let epoch = host_identity(Some("v"), Some([2026.0, 1.0, 1.0, 0.0, 5.0])).expect("identity");
        assert_eq!(
            (epoch.scheme, epoch.ordinal),
            (EngineOrdinalScheme::Epoch, [2026, 1, 1, 0, 5])
        );
        let nodef = host_identity(None, Some([2026.0, 1.0, 1.0, 0.0, 0.0])).expect("identity");
        assert_eq!(
            (nodef.scheme, nodef.text.as_str()),
            (EngineOrdinalScheme::Nodef, "")
        );
        assert!(host_identity(Some("v"), None).is_none());
        assert_eq!(
            serde_json::to_value(StaleEngineKeep::StaleNotOlder).expect("wire"),
            "stale_not_older"
        );
    }
}
