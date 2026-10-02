#![cfg(unix)]
use notify::Watcher;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
#[tokio::test]
async fn real_binary_coordinator_registers_peer_and_cleans_sockets_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let public = dir.path().join("public.sock"); let control = dir.path().join("control.sock");
    let (created, ready) = tokio::sync::oneshot::channel();
    let watched_public = public.clone(); let mut created = Some(created);
    let mut watcher = notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
        if event.is_ok_and(|event| event.paths.contains(&watched_public) && matches!(event.kind, notify::EventKind::Create(_))) && let Some(created) = created.take() { let _ = created.send(()); }
    }).unwrap();
    watcher.watch(dir.path(), notify::RecursiveMode::NonRecursive).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("__PI_INTERNAL_SPAWN", "coordinator")
        .arg(&public).arg(&control).kill_on_drop(true).spawn().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), ready).await.unwrap().unwrap();
    let socket = tokio::net::UnixStream::connect(&control).await.unwrap();
    let (input, mut output) = socket.into_split();
    let registration = serde_json::json!({"type":"register_peer", "protocol":maho_cli::experimental::coordinator::COORDINATOR_PROTOCOL_VERSION, "peerId":"entry-test"});
    output.write_all(format!("{registration}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), tokio::io::BufReader::new(input).read_line(&mut line)).await.unwrap().unwrap();
    let message: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(message["type"], "peer_registered"); assert_eq!(message["peerId"], "entry-test");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id().unwrap() as i32), nix::sys::signal::Signal::SIGTERM).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await.unwrap().unwrap().success());
    assert!(!public.exists()); assert!(!control.exists());
}
