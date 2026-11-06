#![cfg(unix)]
use maho_rpc::{
    host_daemon_paths::{
        create_daemon_directories, create_generation_directory, create_host_daemon_paths,
        generation_paths,
    },
    host_stop::{StopHostResult, stop_host},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[tokio::test]
async fn unknown_owner_is_refused_without_creating_shared_state() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        stop_host("missing", temp.path(), false, true, 100)
            .await
            .unwrap(),
        StopHostResult::Refuse {
            reason: "unknown_owner"
        }
    );
    assert!(!temp.path().join("rpc-host-daemon").exists());
}
#[tokio::test]
async fn live_sessions_refuse_real_stop_and_preserve_registration() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("host.sock");
    let socket = socket.to_str().unwrap();
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let paths = create_host_daemon_paths(socket, temp.path());
    create_daemon_directories(&paths).unwrap();
    let generation = generation_paths(&paths, "one");
    create_generation_directory(&generation).unwrap();
    std::fs::write(&paths.pointer_file, r#"{"instance_id":"one"}"#).unwrap();
    std::fs::write(&generation.pid_file,serde_json::json!({"pid":std::process::id(),"processStartTime":maho_rpc::host_reservations::read_process_start_time(std::process::id()),"socket":socket}).to_string()).unwrap();
    // Probes authenticate first (`get_protocol_info`) and then ask on the same connection, so each connection answers every
    // request line it receives with that request's own id, until the client closes it.
    let serve = async {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "list_sessions" {
                    serde_json::json!({"sessions":[{}]})
                } else {
                    serde_json::json!({"serverVersion":"1","capabilities":[]})
                };
                let response = serde_json::json!({"id":request["id"],"success":true,"data":data});
                write
                    .write_all(
                        maho_rpc::jsonl::serialize_json_line(&response)
                            .unwrap()
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        }
    };
    let stop = stop_host(socket, temp.path(), false, false, 1000);
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::select! {result=stop=>result,()=serve=>unreachable!()}
    })
    .await
    .unwrap();
    assert_eq!(
        result.unwrap(),
        StopHostResult::Refuse {
            reason: "sessions_live"
        }
    );
    assert!(generation.pid_file.exists());
    assert!(paths.pointer_file.exists());
}
/// senpi host-stop.ts: a hard stop records `operator_stop` for the generation before it sends SIGTERM.
#[tokio::test]
async fn a_hard_stop_writes_the_operator_stop_intent_before_signalling() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("host.sock");
    let socket = socket.to_str().unwrap();
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let paths = create_host_daemon_paths(socket, temp.path());
    create_daemon_directories(&paths).unwrap();
    maho_rpc::host_daemon_registration::write_host_registration(
        &paths,
        &maho_rpc::host_daemon_registration::HostRegistration {
            pid,
            process_start_time: maho_rpc::host_reservations::read_process_start_time(pid),
            socket: socket.into(),
            instance_id: "one".into(),
            generation: 0.,
            launch_profile_id: "profile".into(),
            build: None,
        },
    )
    .unwrap();
    let result = maho_rpc::host_stop::stop_host_with(maho_rpc::host_stop::StopHostOptions {
        socket: socket.into(),
        agent_dir: Some(temp.path().into()),
        timeout_ms: Some(200),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        result,
        maho_rpc::host_stop::StopHostOutcome::Stopped { pid }
    );
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(15));
    let intent: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(generation_paths(&paths, "one").stop_intent_file).unwrap(),
    )
    .unwrap();
    assert_eq!(intent["reason"], "operator_stop");
    assert_eq!(intent["signal"], "SIGTERM");
    assert_eq!(intent["targetPid"], pid);
    assert_eq!(
        intent["sender"],
        serde_json::json!({"pid":std::process::id(),"kind":"stop"})
    );
}
