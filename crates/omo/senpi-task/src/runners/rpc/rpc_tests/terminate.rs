//! `runners/rpc/terminate.test.ts`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use super::fake_child::{ChildGuard, fake_command, is_running, spawn_fake_child, wait_for};
use crate::runners::rpc::process::RpcChildProcess;
use crate::runners::rpc::terminate::terminate_rpc_child;
use crate::runners::types::TerminateOptions;

fn delay(ms: u64) -> TerminateOptions {
    TerminateOptions {
        sigkill_delay_ms: Some(ms),
    }
}

fn read_line_ending_with(child: &RpcChildProcess, marker: &str) -> String {
    let stdout = child.take_stdout().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).expect("read stdout") > 0,
            "stdout closed before {marker}"
        );
        let trimmed = line.trim();
        if let Some(index) = trimmed.find(marker) {
            return trimmed[index..].to_string();
        }
    }
}

fn spawn_mode(mode: &str, own_group: bool) -> Arc<RpcChildProcess> {
    Arc::new(RpcChildProcess::spawn_command(fake_command(
        mode,
        &BTreeMap::new(),
        own_group,
    )))
}

#[test]
fn given_a_cooperating_child_when_terminating_then_sigterm_ends_it_without_escalation() {
    let child = spawn_fake_child(&[]);
    let _guard = ChildGuard(Arc::clone(&child));
    read_line_ending_with(&child, "fake_boot");
    terminate_rpc_child(&child, delay(5_000)).expect("terminate");
    assert_eq!(child.wait_exit().signal, Some(libc::SIGTERM));
}

#[test]
fn given_a_term_ignoring_child_when_terminating_then_it_escalates_to_sigkill_within_the_budget() {
    let child = spawn_fake_child(&[("FAKE_IGNORE_TERM", "1")]);
    let _guard = ChildGuard(Arc::clone(&child));
    read_line_ending_with(&child, "ready");
    terminate_rpc_child(&child, delay(150)).expect("terminate");
    assert_eq!(child.wait_exit().signal, Some(libc::SIGKILL));
}

#[test]
fn given_a_non_group_leader_child_when_terminating_then_direct_escalation_remains_available() {
    let child = spawn_mode("descendant", false);
    read_line_ending_with(&child, "READY");
    let pid = child.pid().expect("pid");
    terminate_rpc_child(&child, delay(150)).expect("terminate");
    assert!(!is_running(pid));
}

#[test]
fn given_an_already_exited_child_when_terminating_then_it_resolves_without_throwing() {
    let child = spawn_fake_child(&[]);
    terminate_rpc_child(&child, delay(200)).expect("first terminate");
    terminate_rpc_child(&child, delay(200)).expect("second terminate");
}

#[test]
fn given_a_child_with_a_term_ignoring_descendant_when_terminating_then_the_whole_process_tree_exits()
 {
    let child = spawn_mode("tree", true);
    let _guard = ChildGuard(Arc::clone(&child));
    let line = read_line_ending_with(&child, "PID:");
    let descendant: u32 = line
        .trim_start_matches("PID:")
        .parse()
        .expect("descendant pid");
    let descendant_pid = libc::pid_t::try_from(descendant).expect("pid range");
    let kill_leftover = || {
        if is_running(descendant) {
            // SAFETY: kill(2) on a pid this test spawned.
            unsafe {
                libc::kill(descendant_pid, libc::SIGKILL);
            }
        }
    };
    terminate_rpc_child(&child, delay(150)).expect("terminate");
    let reaped = std::panic::catch_unwind(|| {
        wait_for(Duration::from_secs(2), || !is_running(descendant));
    });
    kill_leftover();
    assert!(
        reaped.is_ok(),
        "descendant {descendant} survived tree termination"
    );
}
