//! `runners/rpc-process.test.ts`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::fake_child::{ChildGuard, spawn_fake_child, wait_for};
use crate::manager::child_handle::ManagedChildHandle;
use crate::runners::rpc::exit_mapping::map_exit_outcome_to_error;
use crate::runners::rpc::handle::RpcChildHandle;
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::{ChildExitOutcome, RpcRunnerSpec};
use crate::runners::{RunnerFailureKind, RunnerOutcome};

const WAIT: Duration = Duration::from_secs(5);

struct Harness {
    runner: RpcProcessRunner,
    captured: Arc<Mutex<Option<RpcSpawnDescriptor>>>,
    children: Arc<Mutex<Vec<ChildGuard>>>,
    state: tempfile::TempDir,
}

impl Harness {
    fn new(heartbeat_interval_ms: Option<u64>) -> Self {
        let captured: Arc<Mutex<Option<RpcSpawnDescriptor>>> = Arc::default();
        let children: Arc<Mutex<Vec<ChildGuard>>> = Arc::default();
        let capture = Arc::clone(&captured);
        let tracked = Arc::clone(&children);
        let runner = RpcProcessRunner::new(RpcProcessRunnerOptions {
            heartbeat_interval_ms,
            model_admission: Some(Arc::new(|_spec: &RpcRunnerSpec| Ok(()))),
            spawn_child: Some(Arc::new(move |descriptor: &RpcSpawnDescriptor| {
                *capture.lock().expect("captured") = Some(descriptor.clone());
                let env: Vec<(&str, &str)> = descriptor
                    .env
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                let child = spawn_fake_child(&env);
                tracked
                    .lock()
                    .expect("children")
                    .push(ChildGuard(Arc::clone(&child)));
                child
            })),
            ..RpcProcessRunnerOptions::default()
        });
        Self {
            runner,
            captured,
            children,
            state: tempfile::tempdir().expect("state dir"),
        }
    }

    fn spec(&self, prompt: &str) -> RpcRunnerSpec {
        RpcRunnerSpec {
            task_id: "st_deadbeef".to_string(),
            cwd: std::env::current_dir()
                .expect("cwd")
                .to_string_lossy()
                .into_owned(),
            state_dir: self.state.path().to_string_lossy().into_owned(),
            prompt: prompt.to_string(),
            ..RpcRunnerSpec::default()
        }
    }

    fn start(&self, prompt: &str) -> Arc<RpcChildHandle> {
        self.runner.start(&self.spec(prompt)).expect("start")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.children.lock().expect("children").clear();
    }
}

fn collect(handle: &RpcChildHandle) -> Arc<Mutex<Vec<Value>>> {
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = Arc::clone(&events);
    let _keep = handle.subscribe_raw(Arc::new(move |event| {
        sink.lock().expect("events").push(event.clone());
    }));
    events
}

fn queued(events: &Mutex<Vec<Value>>, field: &str, text: &str) -> bool {
    events.lock().expect("events").iter().any(|event| {
        event["type"] == "queue_update"
            && event[field]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == text))
    })
}

#[test]
fn given_the_default_rpc_child_spawner_when_started_then_windows_hides_the_child_console() {
    let harness = Harness::new(None);
    let handle = harness.start("hold");
    let child_pid = handle.pid().expect("pid");
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &child_pid.to_string()])
        .output()
        .expect("ps");
    let pgid: i64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("pgid");
    assert_eq!(
        pgid, child_pid,
        "the POSIX child must be detached into its own process group"
    );
    let default_spawn = RpcChildProcess::spawn(&RpcSpawnDescriptor {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "exit 0".to_string()],
        cwd: "/".to_string(),
        env: std::collections::BTreeMap::new(),
    });
    assert!(default_spawn.spawn_error().is_none());
    assert_eq!(default_spawn.wait_exit().code, Some(0));
}

#[test]
fn given_a_completing_child_when_started_then_the_handle_reports_final_text_and_a_clean_exit() {
    let harness = Harness::new(None);
    let handle = harness.start("finish:final answer");
    handle.wait_for_idle();
    let outcome = handle.wait_for_exit();
    assert_eq!(
        handle.last_assistant_text().as_deref(),
        Some("final answer")
    );
    assert!(handle.pid().is_some());
    assert!(matches!(outcome, ChildExitOutcome::Clean { .. }));
    assert_eq!(map_exit_outcome_to_error(&outcome, true), None);
}

#[test]
fn given_a_busy_child_when_steered_then_the_steer_is_acked_while_the_turn_is_in_flight() {
    let harness = Harness::new(None);
    let handle = harness.start("hold");
    let events = collect(&handle);
    handle.steer("mid-course").expect("steer");
    wait_for(WAIT, || queued(&events, "steering", "mid-course"));
}

#[test]
fn given_a_busy_child_when_follow_up_is_sent_then_it_is_routed_as_a_prompt_with_follow_up_streaming_behavior()
 {
    let harness = Harness::new(None);
    let handle = harness.start("hold");
    let events = collect(&handle);
    handle.follow_up("later note").expect("follow up");
    wait_for(WAIT, || queued(&events, "followUp", "later note"));
}

#[test]
fn given_a_child_killed_by_signal_when_it_exits_then_the_outcome_is_killed_and_maps_to_status_error_with_killed_true()
 {
    let harness = Harness::new(None);
    let handle = harness.start("diesignal");
    let outcome = handle.wait_for_exit();
    assert!(matches!(outcome, ChildExitOutcome::Killed { .. }));
    assert_eq!(outcome.facts().signal.as_deref(), Some("SIGKILL"));
    assert!(outcome.facts().pid.is_some());
    let mapped = map_exit_outcome_to_error(&outcome, false).expect("mapped error");
    assert!(mapped.killed);
}

#[test]
fn given_a_child_that_exits_nonzero_before_terminal_when_it_crashes_then_the_outcome_carries_the_stderr_tail()
 {
    let harness = Harness::new(None);
    let failure = harness
        .runner
        .start(&harness.spec("crash:4:boom stderr detail"))
        .err()
        .expect("start must fail");
    assert_eq!(failure.kind, RunnerFailureKind::ChildPromptFailed);
    assert!(
        failure.message.contains("boom stderr detail"),
        "message: {}",
        failure.message
    );
}

#[test]
fn given_a_resident_child_when_heartbeats_poll_then_last_seen_and_session_id_are_recorded() {
    let harness = Harness::new(Some(20));
    let handle = harness.start("hold");
    wait_for(WAIT, || handle.last_seen().is_some());
    assert_eq!(handle.session_id().as_deref(), Some("fake-session"));
}

#[test]
fn given_a_spawn_when_the_descriptor_is_built_then_the_child_gets_an_isolated_session_dir_not_the_real_home()
 {
    let harness = Harness::new(None);
    let spec = harness.spec("hold");
    let handle = harness.runner.start(&spec).expect("start");
    assert!(handle.pid().is_some());
    let descriptor = harness
        .captured
        .lock()
        .expect("captured")
        .clone()
        .expect("descriptor");
    let session_dir = descriptor
        .env
        .get("SENPI_CODING_AGENT_SESSION_DIR")
        .cloned()
        .unwrap_or_default();
    let expected = Path::new(&spec.state_dir)
        .join("sessions")
        .join(&spec.task_id);
    assert!(session_dir.starts_with(&*expected.to_string_lossy()));
    let home = std::env::var("HOME").unwrap_or_default();
    assert!(!session_dir.starts_with(&*Path::new(&home).join(".senpi").to_string_lossy()));
    assert_eq!(descriptor.cwd, spec.cwd);
}

#[test]
fn given_an_idle_resident_child_when_revived_with_a_follow_up_then_wait_for_idle_re_arms_for_the_new_turn_instead_of_the_consumed_first_idle()
 {
    let harness = Harness::new(None);
    let handle = harness.start("first");
    handle.wait_for_idle();
    assert_eq!(handle.last_assistant_text().as_deref(), Some("first"));
    handle.follow_up("second").expect("follow up");
    let (idle_tx, idle_rx) = std::sync::mpsc::channel();
    let waiter = Arc::clone(&handle);
    std::thread::spawn(move || {
        waiter.wait_for_idle();
        let _ = idle_tx.send(());
    });
    assert!(
        idle_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "the consumed first idle must not satisfy the new turn"
    );
    handle.steer("complete").expect("steer");
    idle_rx.recv_timeout(WAIT).expect("next idle");
    assert_eq!(
        handle.last_assistant_text().as_deref(),
        Some("steered-complete")
    );
}

#[test]
fn given_prior_assistant_text_when_a_revived_turn_produces_no_output_then_the_stale_text_cannot_become_a_fresh_success()
 {
    let harness = Harness::new(None);
    let handle = harness.start("first");
    assert_eq!(handle.wait_for_outcome(), RunnerOutcome::completed("first"));
    handle.follow_up("empty-followup").expect("follow up");
    assert_eq!(
        handle.wait_for_outcome(),
        RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            "RPC child turn produced no assistant output"
        )
    );
    assert_eq!(handle.last_assistant_text().as_deref(), Some("first"));
}

fn extension_runner(
    inherited: &[&str],
    seen: &Arc<Mutex<Option<RpcRunnerSpec>>>,
    children: &Arc<Mutex<Vec<ChildGuard>>>,
) -> RpcProcessRunner {
    let seen = Arc::clone(seen);
    let children = Arc::clone(children);
    RpcProcessRunner::new(RpcProcessRunnerOptions {
        inherited_extensions: inherited.iter().map(|entry| (*entry).to_string()).collect(),
        model_admission: Some(Arc::new(|_spec: &RpcRunnerSpec| Ok(()))),
        build_spawn: Some(Arc::new(move |spec: &RpcRunnerSpec| {
            *seen.lock().expect("seen") = Some(spec.clone());
            RpcSpawnDescriptor {
                command: "/bin/true".to_string(),
                cwd: spec.cwd.clone(),
                ..RpcSpawnDescriptor::default()
            }
        })),
        spawn_child: Some(Arc::new(move |_descriptor: &RpcSpawnDescriptor| {
            let child = spawn_fake_child(&[]);
            children
                .lock()
                .expect("children")
                .push(ChildGuard(Arc::clone(&child)));
            child
        })),
        ..RpcProcessRunnerOptions::default()
    })
}

#[test]
fn given_inherited_extensions_and_a_spec_without_its_own_extensions_when_started_then_the_child_spec_carries_the_inherited_entries()
 {
    let harness = Harness::new(None);
    let seen = Arc::default();
    let runner = extension_runner(&["/tmp/mock.ts"], &seen, &harness.children);
    let handle = runner.start(&harness.spec("hello")).expect("start");
    handle.dispose_handle();
    let seen = seen.lock().expect("seen").clone().expect("spec");
    assert_eq!(seen.extensions, Some(vec!["/tmp/mock.ts".to_string()]));
}

#[test]
fn given_a_spec_that_already_carries_extensions_when_started_then_inherited_extensions_do_not_override_them()
 {
    let harness = Harness::new(None);
    let seen = Arc::default();
    let runner = extension_runner(&["/tmp/inherited.ts"], &seen, &harness.children);
    let spec = RpcRunnerSpec {
        extensions: Some(vec!["/tmp/explicit.ts".to_string()]),
        ..harness.spec("hello")
    };
    let handle = runner.start(&spec).expect("start");
    handle.dispose_handle();
    let seen = seen.lock().expect("seen").clone().expect("spec");
    assert_eq!(seen.extensions, Some(vec!["/tmp/explicit.ts".to_string()]));
}
