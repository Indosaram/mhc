//! `runners/rpc/protocol-client.test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::fake_child::{ChildGuard, fake_command, spawn_fake_child, wait_for};
use crate::manager::child_handle::ManagedChildHandle;
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc::protocol_client::{
    RpcClientPort, RpcProtocolClient, RpcProtocolClientOptions,
};
use crate::runners::rpc::terminate::terminate_rpc_child;
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::{
    RpcEntriesResult, RpcRunnerSpec, RpcSpawnSpec, RpcSwitchSessionResult, TerminateOptions,
};

const WAIT: Duration = Duration::from_secs(5);

fn client_for(child: &Arc<RpcChildProcess>) -> RpcProtocolClient {
    RpcProtocolClient::new(Arc::clone(child), RpcProtocolClientOptions::default())
}

fn collect_events(client: &RpcProtocolClient) -> Arc<Mutex<Vec<Value>>> {
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = Arc::clone(&events);
    let _keep = client.on_event(Arc::new(move |event| {
        sink.lock().expect("events").push(event.clone());
    }));
    events
}

fn has_type(events: &Mutex<Vec<Value>>, event_type: &str) -> bool {
    events
        .lock()
        .expect("events")
        .iter()
        .any(|event| event["type"] == event_type)
}

fn spawn_mode(mode: &str) -> Arc<RpcChildProcess> {
    Arc::new(RpcChildProcess::spawn_command(fake_command(
        mode,
        &BTreeMap::new(),
        true,
    )))
}

fn session_runner(child: &Arc<RpcChildProcess>, inherited: Vec<String>) -> RpcProcessRunner {
    let child = Arc::clone(child);
    RpcProcessRunner::new(RpcProcessRunnerOptions {
        heartbeat_interval_ms: Some(60_000),
        build_spawn: Some(Arc::new(|spec: &RpcRunnerSpec| RpcSpawnDescriptor {
            command: "unused".to_string(),
            cwd: spec.cwd.clone(),
            ..RpcSpawnDescriptor::default()
        })),
        spawn_child: Some(Arc::new(move |_descriptor: &RpcSpawnDescriptor| {
            Arc::clone(&child)
        })),
        model_admission: Some(Arc::new(|_spec: &RpcRunnerSpec| Ok(()))),
        inherited_extensions: inherited,
        ..RpcProcessRunnerOptions::default()
    })
}

#[test]
fn w2reattach_given_session_rpc_commands_when_switching_and_reading_entries_then_typed_command_data_is_preserved()
 {
    let child = spawn_mode("session");
    let _guard = ChildGuard(Arc::clone(&child));
    let client = client_for(&child);
    let switched = client.switch_session("/tmp/session.jsonl").expect("switch");
    let entries = client.get_entries(Some("entry-1")).expect("entries");
    let cancelled = client
        .switch_session("/tmp/cancel-session.jsonl")
        .expect("switch cancel");
    assert_eq!(switched, RpcSwitchSessionResult { cancelled: false });
    assert_eq!(
        entries,
        RpcEntriesResult {
            entries: vec![],
            leaf_id: Some("entry-1".to_string())
        }
    );
    assert_eq!(cancelled, RpcSwitchSessionResult { cancelled: true });
}

#[test]
fn w2reattach_given_a_resume_session_path_when_an_rpc_process_starts_then_it_switches_without_replaying_the_prompt()
 {
    let child = spawn_mode("session");
    let _guard = ChildGuard(Arc::clone(&child));
    let runner = session_runner(&child, vec![]);
    let handle = runner
        .start(&RpcRunnerSpec {
            task_id: "st_0000000f".to_string(),
            cwd: std::env::current_dir()
                .expect("cwd")
                .to_string_lossy()
                .into_owned(),
            state_dir: "/tmp/unused".to_string(),
            prompt: "must-not-replay".to_string(),
            resume_session_path: Some("/tmp/session.jsonl".to_string()),
            ..RpcRunnerSpec::default()
        })
        .expect("start");
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = Arc::clone(&events);
    let _keep = handle.subscribe_raw(Arc::new(move |event| {
        sink.lock().expect("events").push(event.clone());
    }));
    let result = handle
        .switch_session("/tmp/session.jsonl")
        .expect("switch supported")
        .expect("switch");
    assert_eq!(result, RpcSwitchSessionResult { cancelled: false });
    handle.dispose_handle();
    let names: Vec<Value> = events
        .lock()
        .expect("events")
        .iter()
        .map(|event| event["name"].clone())
        .collect();
    assert!(!names.contains(&json!("command:prompt")));
}

#[test]
fn w2reattach_given_inherited_rpc_extensions_when_the_process_runner_starts_then_its_effective_spawn_facts_expose_them_for_persistence()
 {
    let child = spawn_mode("session");
    let _guard = ChildGuard(Arc::clone(&child));
    let runner = session_runner(&child, vec!["/tmp/inherited-extension.ts".to_string()]);
    let member_env =
        BTreeMap::from([("SENPI_TASK_MEMBER".to_string(), "run-1::alpha".to_string())]);
    let handle = runner
        .start(&RpcRunnerSpec {
            task_id: "st_0000001f".to_string(),
            cwd: "/tmp".to_string(),
            state_dir: "/tmp/unused".to_string(),
            prompt: "bootstrap".to_string(),
            member_env: Some(member_env.clone()),
            ..RpcRunnerSpec::default()
        })
        .expect("start");
    assert_eq!(
        handle.spawn_spec(),
        Some(RpcSpawnSpec {
            cwd: "/tmp".to_string(),
            extensions: Some(vec!["/tmp/inherited-extension.ts".to_string()]),
            member_env: Some(member_env),
        })
    );
}

#[test]
fn given_two_in_flight_requests_answered_out_of_order_when_correlating_then_each_promise_resolves_by_id()
 {
    let child = spawn_fake_child(&[]);
    let _guard = ChildGuard(Arc::clone(&child));
    let client = Arc::new(client_for(&child));
    let order: Arc<Mutex<Vec<&str>>> = Arc::default();
    let slow = client
        .request(json!({ "type": "prompt", "message": "delay:300:A" }))
        .expect("slow request");
    let fast = client
        .request(json!({ "type": "prompt", "message": "delay:20:B" }))
        .expect("fast request");
    let waiters = [("A", slow), ("B", fast)].map(|(label, pending)| {
        let order = Arc::clone(&order);
        std::thread::spawn(move || {
            let response = pending.wait().expect("response");
            order.lock().expect("order").push(label);
            response
        })
    });
    let responses: Vec<Value> = waiters
        .into_iter()
        .map(|waiter| waiter.join().expect("waiter"))
        .collect();
    assert_eq!(*order.lock().expect("order"), vec!["B", "A"]);
    assert!(responses.iter().all(|response| response["success"] == true));
}

#[test]
fn given_a_completing_turn_when_subscribing_then_agent_lifecycle_events_fan_out_to_every_subscriber()
 {
    let child = spawn_fake_child(&[]);
    let _guard = ChildGuard(Arc::clone(&child));
    let client = client_for(&child);
    let first = collect_events(&client);
    let second = collect_events(&client);
    client
        .send(json!({ "type": "prompt", "message": "hello" }))
        .expect("prompt");
    wait_for(WAIT, || has_type(&first, "agent_end"));
    wait_for(WAIT, || has_type(&second, "agent_end"));
    assert!(has_type(&first, "agent_start"));
}

#[test]
fn given_an_extension_ui_request_when_auto_answering_then_the_child_receives_a_deny_and_never_blocks()
 {
    let child = spawn_fake_child(&[("FAKE_EMIT_UI", "1")]);
    let _guard = ChildGuard(Arc::clone(&child));
    let client = client_for(&child);
    let events = collect_events(&client);
    client
        .send(json!({ "type": "get_state" }))
        .expect("get_state");
    wait_for(WAIT, || has_type(&events, "session_info_changed"));
    let acked = events
        .lock()
        .expect("events")
        .iter()
        .find(|event| event["type"] == "session_info_changed")
        .cloned()
        .expect("ack");
    assert_eq!(acked["name"], "ui:denied");
}

#[test]
fn given_a_malformed_line_when_parsing_then_it_is_reported_and_the_connection_survives() {
    let child = spawn_fake_child(&[("FAKE_EMIT_MALFORMED", "1")]);
    let _guard = ChildGuard(Arc::clone(&child));
    let malformed: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&malformed);
    let client = RpcProtocolClient::new(
        Arc::clone(&child),
        RpcProtocolClientOptions {
            on_malformed_line: Some(Arc::new(move |line: &str, _error: &str| {
                sink.lock().expect("malformed").push(line.to_string());
            })),
            auto_answer_ui: None,
        },
    );
    let events = collect_events(&client);
    client
        .send(json!({ "type": "get_state" }))
        .expect("get_state");
    wait_for(WAIT, || has_type(&events, "agent_start"));
    assert!(
        malformed
            .lock()
            .expect("malformed")
            .contains(&"this-is-not-json".to_string())
    );
}

#[test]
fn given_a_disposed_client_when_sending_then_it_rejects_because_the_process_is_gone() {
    let child = spawn_fake_child(&[]);
    let client = client_for(&child);
    terminate_rpc_child(
        &child,
        TerminateOptions {
            sigkill_delay_ms: Some(200),
        },
    )
    .expect("terminate");
    wait_for(WAIT, || client.exited());
    assert!(client.send(json!({ "type": "get_state" })).is_err());
}

#[test]
fn given_a_verbose_child_stderr_when_chunks_accumulate_then_the_internal_buffer_is_capped_and_tail_reflects_recent_bytes()
 {
    let child = spawn_mode("stderr-flood");
    let _guard = ChildGuard(Arc::clone(&child));
    let client = client_for(&child);
    const STDERR_BUFFER_CAP: usize = 16_384;
    const STDERR_TAIL_CAP: usize = 4_096;
    wait_for(WAIT, || {
        client.stderr_tail().chars().count() >= STDERR_TAIL_CAP
    });
    assert!(client.stderr_buffer_length() <= STDERR_BUFFER_CAP);
    assert!(client.stderr_tail().chars().count() <= STDERR_TAIL_CAP);
    assert!(client.stderr_tail().starts_with('x'));
}
