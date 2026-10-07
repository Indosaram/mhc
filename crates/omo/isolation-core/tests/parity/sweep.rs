use std::sync::Arc;

use isolation_core::test_support::{backend, fixture, TestBackend};
use isolation_core::{hostname, sweep_stale_isolations, BackendRef, OwnerProbe, OwnerStatus, SweepOptions};

struct DeadUnless {
    alive_pid: u32,
    unknown_pid: u32,
}

impl OwnerProbe for DeadUnless {
    fn pid_alive(&self, pid: u32, _start_identity: Option<&str>) -> OwnerStatus {
        if pid == self.alive_pid {
            OwnerStatus::Alive
        } else if pid == self.unknown_pid {
            OwnerStatus::Unknown
        } else {
            OwnerStatus::Dead
        }
    }
}

fn write_marker(base: &std::path::Path, kind: &str, pid: u32, host: &str) {
    std::fs::create_dir_all(base.join("m")).expect("merged dir");
    std::fs::write(
        base.join(".omo-isolation-owner.json"),
        format!(
            "{{\"id\":\"{kind}\",\"hostname\":\"{host}\",\"created_at\":0,\"host\":{{\"pid\":{pid},\"start_identity\":null}}}}"
        ),
    )
    .expect("owner marker");
    std::fs::write(
        base.join(".omo-isolation-backend.json"),
        "{\"backend\":\"rcopy\"}",
    )
    .expect("backend marker");
}

#[test]
fn sweep_stops_dead_backends_and_retains_live_foreign_unknown_retained_and_skips_symlinks() {
    let f = fixture();
    let sweep_root = f.root.join("wt");
    std::fs::create_dir_all(&sweep_root).expect("sweep root");
    for (index, kind) in ["dead", "live", "foreign", "unknown", "retained"]
        .iter()
        .enumerate()
    {
        let suffix = if *kind == "retained" {
            ".retained-1-x"
        } else {
            ""
        };
        let name = format!("t{index:010}{suffix}");
        let base = sweep_root.join(name);
        let host = if *kind == "foreign" {
            "foreign.example".to_string()
        } else {
            hostname()
        };
        write_marker(&base, kind, (index + 1) as u32, &host);
    }
    std::os::unix::fs::symlink(&f.repo_root, sweep_root.join("t9999999999"))
        .expect("symlink");
    let stopped: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_sink = Arc::clone(&stopped);
    let sweeper: BackendRef = Arc::new(TestBackend {
        stop_fn: Arc::new(move |merged| {
            std::fs::metadata(merged)?;
            stop_sink
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(merged.to_path_buf());
            Ok(())
        }),
        ..TestBackend::new()
    });
    let result = sweep_stale_isolations(
        &[sweep_root.clone()],
        &SweepOptions {
            backends: vec![sweeper],
            probe: Some(Arc::new(DeadUnless {
                alive_pid: 2,
                unknown_pid: 4,
            })),
            now: None,
        },
    )
    .expect("sweep");
    assert_eq!(result.reclaimed, vec![sweep_root.join("t0000000000")]);
    assert_eq!(
        *stopped.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![sweep_root.join("t0000000000").join("m")]
    );
    assert_eq!(result.kept.len(), 4);
    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].path, sweep_root.join("t9999999999"));
    assert_eq!(result.skipped[0].reason, "not a directory");
    std::fs::metadata(&f.repo_root).expect("source repo preserved");
}

#[test]
fn sweep_keeps_dead_trees_when_backend_teardown_fails() {
    let f = fixture();
    let base = f.root.join("t0123456789");
    write_marker(&base, "dead", 1, &hostname());
    let sweeper: BackendRef = Arc::new(TestBackend {
        stop_fn: Arc::new(|_merged| Err(isolation_core::IsolationError::other("unmount denied"))),
        ..TestBackend::new()
    });
    let result = sweep_stale_isolations(
        &[f.root.clone()],
        &SweepOptions {
            backends: vec![sweeper],
            probe: Some(Arc::new(DeadUnless {
                alive_pid: 0,
                unknown_pid: 0,
            })),
            now: None,
        },
    )
    .expect("sweep");
    assert!(result.reclaimed.is_empty());
    assert!(result
        .skipped
        .iter()
        .any(|entry| entry.path == base && entry.reason.contains("unmount denied")));
    std::fs::metadata(&base).expect("tree preserved");
}

#[test]
fn sweep_reports_nothing_for_a_root_that_does_not_exist() {
    let f = fixture();
    let result = sweep_stale_isolations(
        &[f.root.join("absent")],
        &SweepOptions {
            backends: Vec::new(),
            probe: Some(Arc::new(DeadUnless {
                alive_pid: 0,
                unknown_pid: 0,
            })),
            now: None,
        },
    )
    .expect("sweep");
    assert!(result.reclaimed.is_empty());
    assert!(result.kept.is_empty());
    assert!(result.skipped.is_empty());
    let _ = backend();
}
