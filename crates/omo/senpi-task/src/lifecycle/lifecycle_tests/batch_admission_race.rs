//! `lifecycle/batch-admission-race.test.ts` (+ `__fixtures__/batch-admission-race-child.ts`).
//!
//! The race needs two real OS processes (distinct pids, independent in-process state). The test
//! re-executes this test binary twice, selecting only `race_child`, which acts as the fixture
//! when `SENPI_TASK_RACE_CHILD_STATE_DIR` is set and is a no-op otherwise. Instead of the TS
//! ready/barrier files polled every 5 ms, each child prints `READY` and blocks on stdin; the
//! parent releases both by writing to their stdin once both are ready.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::lifecycle::admission_lease::AdmissionLeaseTimingOverrides;
use crate::lifecycle::residency::{
    BatchAdmissionOptions, BatchAdmissionOutcome, admit_suspended_batch,
};
use crate::lifecycle::{LifecycleDeps, resolve_context};
use crate::state::{ResidencyState::*, TaskStatus::*};

const STATE_DIR_ENV: &str = "SENPI_TASK_RACE_CHILD_STATE_DIR";
const CHILD_TEST: &str = "lifecycle::lifecycle_tests::batch_admission_race::race_child";

fn store_at(state_dir: &std::path::Path) -> TaskRecordStore {
    TaskRecordStore::new(&StateDirConfig {
        project_dir: state_dir.to_path_buf(),
        task_state_dir: Some(state_dir.to_path_buf()),
    })
}

/// Child-process fixture: admits one batch for `parent-1` with cap 5 after the parent's barrier.
#[test]
fn race_child() {
    let Ok(state_dir) = std::env::var(STATE_DIR_ENV) else {
        return;
    };
    let store = Arc::new(store_at(std::path::Path::new(&state_dir)));
    let context = resolve_context(LifecycleDeps::new(
        store,
        Arc::new(FakeRegistry::default()),
        settings(json!({ "residency_max_children": 5 })),
    ));
    // Direct writes bypass libtest's output capture, so the parent sees them unbuffered.
    let mut stdout = std::io::stdout();
    writeln!(stdout, "READY").expect("write ready");
    stdout.flush().expect("flush ready");
    let mut go = [0_u8; 1];
    std::io::stdin().read_exact(&mut go).expect("barrier");
    let options = BatchAdmissionOptions {
        timing: AdmissionLeaseTimingOverrides {
            renew_ms: Some(50),
            stale_ms: Some(150),
            acquire_timeout_ms: Some(10_000),
            retry_ms: Some(10),
        },
        ..BatchAdmissionOptions::default()
    };
    let result = admit_suspended_batch(&context, "parent-1", &options).expect("batch");
    let claimed: Vec<&str> = result
        .outcomes
        .iter()
        .filter(|outcome| matches!(outcome, BatchAdmissionOutcome::Claimed { .. }))
        .map(BatchAdmissionOutcome::task_id)
        .collect();
    let line = json!({ "pid": std::process::id(), "lease": format!("{:?}", result.lease), "claimed": claimed });
    writeln!(stdout, "{line}").expect("write result");
    stdout.flush().expect("flush result");
}

#[test]
fn two_processes_never_exceed_the_cap_or_duplicate_claims() {
    let temp = temp_store();
    let ids = [
        "st_00000090",
        "st_00000091",
        "st_00000092",
        "st_00000093",
        "st_00000094",
        "st_00000095",
        "st_00000096",
        "st_00000097",
    ];
    for (index, id) in ids.into_iter().enumerate() {
        let offset = i64::try_from(index).expect("index");
        seed_record(
            &temp.store,
            Seed {
                task_id: id,
                status: Some(if index % 2 == 0 { Running } else { Completed }),
                residency_state: Some(if index % 3 == 0 {
                    RpcDetached
                } else {
                    PersistedOnly
                }),
                updated_at: Some(iso(offset * 10)),
                ..Seed::default()
            },
        );
    }
    let exe = std::env::current_exe().expect("test binary");
    let mut children: Vec<_> = (0..2)
        .map(|_| {
            Command::new(&exe)
                .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
                .env(STATE_DIR_ENV, temp.store.state_dir())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn race child")
        })
        .collect();
    let mut readers: Vec<_> = children
        .iter_mut()
        .map(|child| BufReader::new(child.stdout.take().expect("child stdout")))
        .collect();
    for reader in &mut readers {
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                reader.read_line(&mut line).expect("read child"),
                0,
                "child exited before READY"
            );
            // libtest prefixes the first output line with "test <name> ... ".
            if line.trim_end().ends_with("READY") {
                break;
            }
        }
    }
    for child in &mut children {
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(b"g")
            .expect("release barrier");
    }
    let mut results = Vec::new();
    for (child, mut reader) in children.into_iter().zip(readers) {
        let mut rest = String::new();
        reader.read_to_string(&mut rest).expect("child output");
        let output = child.wait_with_output().expect("child exit");
        assert!(
            output.status.success(),
            "stdout:\n{rest}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let line = rest
            .lines()
            .rfind(|line| line.starts_with('{'))
            .unwrap_or_else(|| panic!("child did not emit a JSON result:\n{rest}"));
        results.push(serde_json::from_str::<Value>(line).expect("child json"));
    }
    let claimed: Vec<String> = results
        .iter()
        .flat_map(|result| result["claimed"].as_array().expect("claimed").clone())
        .map(|id| id.as_str().expect("id").to_string())
        .collect();
    let unique: std::collections::HashSet<&String> = claimed.iter().collect();
    assert_eq!(unique.len(), claimed.len());
    assert_eq!(claimed.len(), 5);
    let fresh = store_at(temp.store.state_dir());
    let records: Vec<TaskRecord> = fresh
        .list()
        .expect("list")
        .records
        .into_iter()
        .filter(|record| record.parent_session_id == "parent-1")
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.residency_state == Resident)
            .count(),
        5
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record.residency_state, PersistedOnly | RpcDetached))
            .count(),
        3
    );
    let child_pids: std::collections::HashSet<i64> = results
        .iter()
        .map(|result| result["pid"].as_i64().expect("pid"))
        .collect();
    assert_eq!(child_pids.len(), 2);
    for record in records
        .iter()
        .filter(|record| record.residency_state == Resident)
    {
        assert!(child_pids.contains(&record.host_pid.unwrap_or(-1)));
    }
}
