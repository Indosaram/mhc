//! Re-pointing an endpoint whose pointer names no generation (senpi `host-pointer-election.ts`, senpi#1911).
//!
//! A prune drops a pointer that names a dead generation, and a live generation can then be left with no
//! pointer at all: an older build's registration raced a newer prune, or a supervisor died after its
//! successor registered. Every reader of the pointer (`host stop`, `host handoff`, `host status`) then reads
//! "no owner" while a healthy host serves the socket, and refuses `unknown_owner`.
//!
//! The pointer is given back only to the ONE generation whose record proves it: the record names this
//! endpoint's public socket and its own instance id, and its pid is alive with the recorded start time. A
//! generation someone is stopping or draining is never a candidate. Zero or several candidates leave the
//! directory without a pointer, and the caller reports which ones it saw. The pointer is created with
//! `link()`, which never replaces a file: a registration that wrote a pointer meanwhile wins.
use crate::{
    host_daemon_paths::{HOST_DAEMON_LAYOUT, HostDaemonPaths, generation_paths},
    host_daemon_state::{parse_json, read_file_or_undefined, write_state_file},
};
use serde::Serialize;
use serde_json::Value;
use std::fs;

/// What the pointer names after an election, for `host status` to report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPointerState {
    /// `current`, `elected` or `missing`.
    pub state: &'static str,
    /// `None` exactly when `state` is `missing`.
    pub instance_id: Option<String>,
    pub candidates: Vec<String>,
}

impl HostPointerState {
    pub fn current(instance_id: String) -> Self {
        Self {
            state: "current",
            instance_id: Some(instance_id),
            candidates: Vec::new(),
        }
    }
    pub fn missing(candidates: Vec<String>) -> Self {
        Self {
            state: "missing",
            instance_id: None,
            candidates,
        }
    }
}

struct Candidate {
    instance_id: String,
    writer: Option<Value>,
}

/// Elects the one proven live generation when the pointer is absent. `write: false` reports what an
/// election would see without writing (`host status --all`, which never mutates the directory).
pub fn elect_pointer(paths: &HostDaemonPaths, socket: &str, write: bool) -> HostPointerState {
    if let Some(text) = read_file_or_undefined(&paths.pointer_file).ok().flatten() {
        return match parse_json(Some(&text))
            .and_then(|pointer| pointer.get("instance_id")?.as_str().map(str::to_owned))
        {
            Some(instance_id) => HostPointerState::current(instance_id),
            None => HostPointerState::missing(Vec::new()),
        };
    }
    let candidates = proven_live_generations(paths, socket);
    let ids: Vec<String> = candidates.iter().map(|c| c.instance_id.clone()).collect();
    let [only] = candidates.as_slice() else {
        return HostPointerState::missing(ids);
    };
    if !write {
        return HostPointerState::missing(ids);
    }
    if link_pointer(paths, only) {
        return HostPointerState {
            state: "elected",
            instance_id: Some(only.instance_id.clone()),
            candidates: ids,
        };
    }
    elect_pointer(paths, socket, false)
}

fn proven_live_generations(paths: &HostDaemonPaths, socket: &str) -> Vec<Candidate> {
    let mut ids: Vec<String> = fs::read_dir(&paths.generations_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    let mut found = Vec::new();
    for instance_id in ids {
        let generation = generation_paths(paths, &instance_id);
        if generation.stop_intent_file.exists() || generation.drain_request_file.exists() {
            continue;
        }
        let text = read_file_or_undefined(&generation.pid_file).ok().flatten();
        let Some(record) = parse_json(text.as_deref()) else {
            continue;
        };
        let Some(pid) = record
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        let Some(start) = record.get("processStartTime").and_then(Value::as_str) else {
            continue;
        };
        if record.get("instance_id").and_then(Value::as_str) != Some(instance_id.as_str()) {
            continue;
        }
        let same_endpoint = record
            .get("socket")
            .and_then(Value::as_str)
            .is_some_and(|recorded| {
                crate::host_ensure::normalize_socket_path(recorded)
                    == crate::host_ensure::normalize_socket_path(socket)
            });
        if !same_endpoint {
            continue;
        }
        // senpi `processMatchesPidFile`: the start must match AND the process must still run (a zombie keeps its
        // /proc stat, and so its start, until it is reaped).
        if crate::host_reservations::read_process_start_time(pid).as_deref() == Some(start)
            && crate::host_reservations::process_is_running(pid)
        {
            found.push(Candidate {
                instance_id,
                writer: record.get("writer").cloned(),
            });
        }
    }
    found
}

/// Creates the pointer only where none exists; false when another writer's pointer is already there.
fn link_pointer(paths: &HostDaemonPaths, candidate: &Candidate) -> bool {
    let staged = std::path::PathBuf::from(format!(
        "{}.elect-{}-{}.tmp",
        paths.pointer_file.display(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut pointer = serde_json::json!({
        "layout": HOST_DAEMON_LAYOUT,
        "instance_id": candidate.instance_id,
        "generation_dir": generation_paths(paths, &candidate.instance_id).relative_dir,
    });
    if let Some(writer) = candidate
        .writer
        .as_ref()
        .filter(|writer| writer.is_object())
    {
        pointer["writer"] = writer.clone();
    }
    let linked = write_state_file(&staged, &pointer).is_ok()
        && fs::hard_link(&staged, &paths.pointer_file).is_ok();
    let _ = fs::remove_file(&staged);
    linked
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, HostDaemonPaths, String) {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("host.sock").to_string_lossy().into_owned();
        let paths = crate::host_daemon_paths::create_host_daemon_paths(&socket, temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        (temp, paths, socket)
    }
    fn live_generation(paths: &HostDaemonPaths, id: &str, socket: &str) {
        let generation = generation_paths(paths, id);
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        let pid = std::process::id();
        fs::write(
            generation.pid_file,
            serde_json::json!({"pid":pid,"processStartTime":crate::host_reservations::read_process_start_time(pid),"instance_id":id,"socket":socket,"writer":{"pid":pid}}).to_string(),
        )
        .unwrap();
    }
    #[test]
    fn a_present_pointer_is_current_and_never_rewritten() {
        let (_temp, paths, socket) = setup();
        fs::write(&paths.pointer_file, r#"{"instance_id":"one"}"#).unwrap();
        assert_eq!(
            elect_pointer(&paths, &socket, true),
            HostPointerState::current("one".into())
        );
    }
    #[cfg(unix)]
    #[test]
    fn the_single_proven_live_generation_is_elected_once() {
        let (_temp, paths, socket) = setup();
        live_generation(&paths, "live", &socket);
        assert_eq!(
            elect_pointer(&paths, &socket, false),
            HostPointerState::missing(vec!["live".into()])
        );
        assert!(!paths.pointer_file.exists());
        let elected = elect_pointer(&paths, &socket, true);
        assert_eq!(elected.state, "elected");
        assert_eq!(elected.instance_id.as_deref(), Some("live"));
        let pointer: Value =
            serde_json::from_str(&fs::read_to_string(&paths.pointer_file).unwrap()).unwrap();
        assert_eq!(pointer["instance_id"], "live");
        assert_eq!(pointer["layout"], HOST_DAEMON_LAYOUT);
        assert!(pointer["writer"].is_object());
        assert_eq!(elect_pointer(&paths, &socket, true).state, "current");
    }
    #[cfg(unix)]
    #[test]
    fn several_candidates_or_a_stopping_generation_elect_nothing() {
        let (_temp, paths, socket) = setup();
        live_generation(&paths, "a", &socket);
        live_generation(&paths, "b", &socket);
        assert_eq!(
            elect_pointer(&paths, &socket, true),
            HostPointerState::missing(vec!["a".into(), "b".into()])
        );
        fs::write(generation_paths(&paths, "b").stop_intent_file, "{}").unwrap();
        live_generation(&paths, "c", "/elsewhere.sock");
        assert_eq!(
            elect_pointer(&paths, &socket, true).instance_id.as_deref(),
            Some("a")
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_generation_is_not_proven_live() {
        let (_temp, paths, socket) = setup();
        // The child exits unreaped, so it stays a zombie with a readable start time; the guard reaps it on drop.
        let child = crate::host_reservations::ZombieChild::spawn();
        let pid = child.id();
        let start = crate::host_reservations::read_process_start_time(pid);
        assert!(start.is_some(), "a zombie keeps its start time");
        let generation = generation_paths(&paths, "zombie");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        fs::write(
            generation.pid_file,
            serde_json::json!({"pid":pid,"processStartTime":start,"instance_id":"zombie","socket":socket})
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            elect_pointer(&paths, &socket, true),
            HostPointerState::missing(Vec::new())
        );
    }
}
