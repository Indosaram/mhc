use std::path::{Path, PathBuf};
use std::sync::Arc;

use isolation_core::test_support::{fixture, Fixture};
use isolation_core::{
    hostname, read_owner_liveness, write_owner_marker, IsolationOwner, OwnerChild, OwnerLiveness,
    OwnerProbe, OwnerStatus,
};

const NOW: u64 = 2_000_000;

type PidProbeFn = Arc<dyn Fn(u32, Option<&str>) -> OwnerStatus + Send + Sync>;
type SessionProbeFn = Arc<dyn Fn(&str, &str) -> OwnerStatus + Send + Sync>;

struct Probe {
    pid: PidProbeFn,
    session: Option<SessionProbeFn>,
}

impl Probe {
    fn dead() -> Self {
        Probe {
            pid: Arc::new(|_, _| OwnerStatus::Dead),
            session: None,
        }
    }
}

impl OwnerProbe for Probe {
    fn pid_alive(&self, pid: u32, start_identity: Option<&str>) -> OwnerStatus {
        (self.pid)(pid, start_identity)
    }

    fn host_session_alive(&self, socket: &str, session_path: &str) -> Option<OwnerStatus> {
        self.session.as_ref().map(|probe| probe(socket, session_path))
    }
}

fn set_mtime_ms(path: &Path, millis: u64) {
    let time = filetime::FileTime::from_unix_time((millis / 1000) as i64, 0);
    filetime::set_file_mtime(path, time).expect("mtime");
}

fn marked(name: &str, child: Option<&str>, host: &str) -> (Fixture, PathBuf) {
    let f = fixture();
    let base = f.root.join(name);
    std::fs::create_dir_all(&base).expect("base dir");
    let child_json = match child {
        Some(child) => format!(",\"child\":{child}"),
        None => String::new(),
    };
    std::fs::write(
        base.join(".omo-isolation-owner.json"),
        format!(
            "{{\"id\":\"id\",\"hostname\":\"{host}\",\"created_at\":0,\"host\":{{\"pid\":101,\"start_identity\":\"proc-start-epoch:10\"}}{child_json}}}"
        ),
    )
    .expect("owner marker");
    set_mtime_ms(&base, 0);
    (f, base)
}

#[test]
fn dead_host_is_dead() {
    let (_f, base) = marked("t0123456789", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Dead
    );
}

#[test]
fn recycled_host_start_identity_is_passed_to_the_probe_and_rejected() {
    let probe = Probe {
        pid: Arc::new(|pid, identity| {
            if pid == 101 && identity == Some("proc-start-epoch:20") {
                OwnerStatus::Alive
            } else {
                OwnerStatus::Dead
            }
        }),
        session: None,
    };
    let (_f, base) = marked("t0123456789", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Dead
    );
}

#[test]
fn malformed_old_marker_obeys_the_grace_period() {
    let (_f, base) = marked("t0123456789", None, &hostname());
    std::fs::write(base.join(".omo-isolation-owner.json"), "{").expect("write");
    set_mtime_ms(&base, 0);
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Reclaimable
    );
}

#[test]
fn malformed_young_marker_obeys_the_grace_period() {
    let (_f, base) = marked("t0123456789", None, &hostname());
    std::fs::write(base.join(".omo-isolation-owner.json"), "{").expect("write");
    set_mtime_ms(&base, NOW);
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Creating
    );
}

#[test]
fn missing_marker_is_only_reclaimable_after_grace() {
    let f = fixture();
    let base = f.root.join("t0123456789");
    std::fs::create_dir_all(&base).expect("base dir");
    set_mtime_ms(&base, NOW);
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Creating
    );
    set_mtime_ms(&base, 0);
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Reclaimable
    );
}

#[test]
fn foreign_host_is_never_probed() {
    let probe = Probe {
        pid: Arc::new(|_, _| panic!("must not probe foreign pid")),
        session: None,
    };
    let (_f, base) = marked("t0123456789", None, "foreign.example");
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Foreign
    );
}

#[test]
fn retained_directory_is_never_probed() {
    let probe = Probe {
        pid: Arc::new(|_, _| panic!("must not probe retained owner")),
        session: None,
    };
    let (_f, base) = marked("t0123456789.retained-1-ab", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Retained
    );
}

#[test]
fn young_creating_directory_with_dead_owner_stays_creating() {
    let (_f, base) = marked("t0123456789.creating-101", None, &hostname());
    set_mtime_ms(&base, NOW);
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Creating
    );
}

#[test]
fn old_creating_directory_with_alive_owner_stays_creating() {
    let probe = Probe {
        pid: Arc::new(|_, _| OwnerStatus::Alive),
        session: None,
    };
    let (_f, base) = marked("t0123456789.creating-101", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Creating
    );
}

#[test]
fn old_creating_directory_with_dead_owner_becomes_reclaimable() {
    let (_f, base) = marked("t0123456789.creating-101", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Reclaimable
    );
}

#[test]
fn live_process_child_preserves_a_dead_host_tree() {
    let probe = Probe {
        pid: Arc::new(|pid, _| {
            if pid == 202 {
                OwnerStatus::Alive
            } else {
                OwnerStatus::Dead
            }
        }),
        session: None,
    };
    let (_f, base) = marked(
        "t0123456789",
        Some("{\"kind\":\"process\",\"pid\":202,\"start_identity\":\"proc-start-epoch:30\"}"),
        &hostname(),
    );
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Live
    );
}

#[test]
fn host_session_alive_with_dead_host_returns_live() {
    let probe = Probe {
        pid: Arc::new(|_, _| OwnerStatus::Dead),
        session: Some(Arc::new(|socket, session| {
            assert_eq!(socket, "/socket");
            assert_eq!(session, "/session");
            OwnerStatus::Alive
        })),
    };
    let (_f, base) = marked(
        "t0123456789",
        Some("{\"kind\":\"host-session\",\"socket\":\"/socket\",\"session_path\":\"/session\"}"),
        &hostname(),
    );
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Live
    );
}

#[test]
fn host_session_unknown_with_dead_host_returns_unknown() {
    let probe = Probe {
        pid: Arc::new(|_, _| OwnerStatus::Dead),
        session: Some(Arc::new(|_, _| OwnerStatus::Unknown)),
    };
    let (_f, base) = marked(
        "t0123456789",
        Some("{\"kind\":\"host-session\",\"socket\":\"/socket\",\"session_path\":\"/session\"}"),
        &hostname(),
    );
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Unknown
    );
}

#[test]
fn host_session_dead_with_dead_host_returns_dead() {
    let probe = Probe {
        pid: Arc::new(|_, _| OwnerStatus::Dead),
        session: Some(Arc::new(|_, _| OwnerStatus::Dead)),
    };
    let (_f, base) = marked(
        "t0123456789",
        Some("{\"kind\":\"host-session\",\"socket\":\"/socket\",\"session_path\":\"/session\"}"),
        &hostname(),
    );
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Dead
    );
}

#[test]
fn missing_host_session_probe_is_unknown_never_dead() {
    let (_f, base) = marked(
        "t0123456789",
        Some("{\"kind\":\"host-session\",\"socket\":\"/socket\",\"session_path\":\"/session\"}"),
        &hostname(),
    );
    assert_eq!(
        read_owner_liveness(&base, &Probe::dead(), NOW).expect("liveness"),
        OwnerLiveness::Unknown
    );
}

#[test]
fn unknown_pid_ownership_wins_over_old_age() {
    let probe = Probe {
        pid: Arc::new(|_, _| OwnerStatus::Unknown),
        session: None,
    };
    let (_f, base) = marked("t0123456789.creating-101", None, &hostname());
    assert_eq!(
        read_owner_liveness(&base, &probe, NOW).expect("liveness"),
        OwnerLiveness::Unknown
    );
}

#[test]
fn records_host_and_child_identities_separately() {
    let f = fixture();
    write_owner_marker(
        &f.root,
        "task",
        &IsolationOwner {
            host: isolation_core::HostOwner { pid: 101 },
            child: Some(OwnerChild::Process { pid: 202 }),
        },
        &|pid| Some(format!("test:{pid}")),
    )
    .expect("marker");
    let text =
        std::fs::read_to_string(f.root.join(".omo-isolation-owner.json")).expect("marker text");
    let marker: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(
        marker.get("host").cloned().unwrap_or_default(),
        serde_json::json!({"pid": 101, "start_identity": "test:101"})
    );
    assert_eq!(
        marker.get("child").cloned().unwrap_or_default(),
        serde_json::json!({"kind": "process", "pid": 202, "start_identity": "test:202"})
    );
}
