use std::sync::Arc;
use maho_agent::harness::{context::BACKGROUND_CONTEXT, env::nodejs::NodeExecutionEnv, session::jsonl::{JsonlSessionRepo, JsonlSessionRepoOptions, JsonlSessionCreateOptions}};
#[cfg(unix)]
#[tokio::test]
async fn process_worker_uses_json_pipes_and_closes_when_stopped() {
    use maho_cli::experimental::mini::server::process_worker_factory;
    let script = "printf '%s\\n' '{\"kind\":\"announce\",\"services\":[\"worker\"]}'; while IFS= read -r line; do case \"$line\" in *'\"kind\":\"call\"'*) printf '%s\\n' '{\"kind\":\"result\",\"id\":1,\"result\":{\"sessionId\":\"child\"}}';; esac; done";
    let spawn = process_worker_factory("/bin/sh".into(), vec!["-c".into(), script.into(), "worker".into()], "/sessions".into());
    let worker = spawn(Some("child".into()), "/cwd".into()).await.unwrap();
    let (closed, closure) = tokio::sync::oneshot::channel();
    let closed = std::sync::Mutex::new(Some(closed));
    worker.peer.on_close(move || { if let Some(closed) = closed.lock().unwrap().take() { let _ = closed.send(()); } });
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), worker.peer.call("worker.describe", vec![])).await.unwrap().unwrap();
    assert_eq!(result["sessionId"], "child");
    (worker.stop)();
    tokio::time::timeout(std::time::Duration::from_secs(5), closure).await.unwrap().unwrap();
}
#[tokio::test]
async fn server_lists_real_repository_metadata_without_opening_agent_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let root = dir.path().join("sessions").to_string_lossy().into_owned();
    let env = Arc::new(NodeExecutionEnv::new(&cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env, sessions_root: root.clone(), now: Some(Arc::new(|| 42)) });
    let session = repo.create(JsonlSessionCreateOptions { id: Some("known-session".to_owned()), cwd: cwd.clone(), parent_session_id: None }, &BACKGROUND_CONTEXT).await.unwrap();
    let expected = session.metadata().id.clone();
    repo.close(&BACKGROUND_CONTEXT).await;
    let sessions = maho_cli::experimental::mini::server::list_sessions(&root, &cwd).await.unwrap();
    assert_eq!(sessions.len(), 1); assert_eq!(sessions[0].id, expected); assert_eq!(sessions[0].cwd, cwd); assert_eq!(sessions[0].created_at, 42.0);
    assert!(maho_cli::experimental::mini::server::list_sessions(&dir.path().join("missing").to_string_lossy(), &cwd).await.unwrap().is_empty());
}
#[tokio::test]
async fn worker_opens_existing_session_by_id_and_creates_without_id() {
    use maho_cli::experimental::mini::worker::open_session;
    let dir = tempfile::tempdir().unwrap(); let cwd = dir.path().to_string_lossy().into_owned();
    let root = dir.path().join("sessions").to_string_lossy().into_owned();
    let env = Arc::new(NodeExecutionEnv::new(&cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env.clone(), sessions_root: root.clone(), now: Some(Arc::new(|| 42)) });
    let created = open_session(&repo, None, &cwd, &BACKGROUND_CONTEXT).await.unwrap();
    let id = created.metadata().id.clone(); repo.close(&BACKGROUND_CONTEXT).await;
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env, sessions_root: root, now: None });
    assert_eq!(open_session(&repo, Some(&id), "/different", &BACKGROUND_CONTEXT).await.unwrap().metadata().id, id);
    assert_eq!(open_session(&repo, Some("missing"), &cwd, &BACKGROUND_CONTEXT).await.err().unwrap(), "Unknown session: missing");
    repo.close(&BACKGROUND_CONTEXT).await;
}
#[cfg(unix)]
#[tokio::test]
async fn routing_deduplicates_attaches_forwards_calls_and_addresses_worker_events() {
    use maho_cli::experimental::mini::{server::*, shared::{rpc::*, protocol::{WORKER, LANE}}};
    use std::collections::HashMap;
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap(); let socket = dir.path().join("mini.sock");
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let workers = Arc::new(std::sync::Mutex::new(Vec::<Arc<RpcPeer>>::new()));
    let (stopped, mut stops) = tokio::sync::mpsc::unbounded_channel();
    let spawned = started.clone(); let kept_workers = workers.clone();
    let spawn: SpawnWorker = Arc::new(move |id, _| {
        spawned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (left, right) = tokio::io::duplex(4096);
        let (lr, lw) = tokio::io::split(left); let (rr, rw) = tokio::io::split(right);
        let host = Arc::new(create_peer(lr, lw, PeerOptions { dead_ms: 0, ..Default::default() }));
        let worker = Arc::new(create_peer(rr, rw, PeerOptions { dead_ms: 0, ..Default::default() }));
        let describe: Handler = Arc::new(move |_, _| { let id = id.clone(); Box::pin(async move { Ok(json!({"sessionId":id.unwrap_or_else(|| "new".to_owned())})) }) });
        let echo: Handler = Arc::new(|args, _| Box::pin(async move { Ok(args[0].clone()) }));
        worker.provide(WORKER, HashMap::from([("describe".to_owned(), describe)]));
        worker.provide(LANE, HashMap::from([("echo".to_owned(), echo)]));
        kept_workers.lock().expect("test workers").push(worker.clone());
        let stopped = stopped.clone();
        Box::pin(async move { Ok(SpawnedWorker { peer: host, stop: Box::new(move || { worker.close(); let _ = stopped.send(()); }) }) })
    });
    let shutdown = maho_ai::utils::abort::AbortController::new(); let signal = shutdown.signal();
    let (ready, readiness) = tokio::sync::oneshot::channel();
    let root = dir.path().join("sessions").to_string_lossy().into_owned();
    let runtime = run_server(&socket, &root, dir.path().to_str().unwrap(), spawn, &signal, Some(ready));
    let scenario = async {
        readiness.await.unwrap();
        let socket1 = tokio::net::UnixStream::connect(&socket).await.unwrap(); let (r1, w1) = socket1.into_split();
        let first = create_peer(r1, w1, Default::default());
        let socket2 = tokio::net::UnixStream::connect(&socket).await.unwrap(); let (r2, w2) = socket2.into_split();
        let second = create_peer(r2, w2, Default::default());
        assert_eq!(first.call("lane.echo", vec![json!(1)]).await.unwrap_err(), "Not attached to a session");
        let (a, b) = tokio::join!(first.call("sessions.attach", vec![json!("same"), json!("/cwd"), json!("one")]), second.call("sessions.attach", vec![json!("same"), json!("/cwd"), json!("two")]));
        assert_eq!(a.unwrap(), "same"); assert_eq!(b.unwrap(), "same");
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(first.call("lane.echo", vec![json!(42)]).await.unwrap(), 42);
        let (first_event, mut first_events) = tokio::sync::mpsc::unbounded_channel();
        let (second_event, mut second_events) = tokio::sync::mpsc::unbounded_channel();
        first.on_event(move |_, payload, _| { let _ = first_event.send(payload.clone()); });
        second.on_event(move |_, payload, _| { let _ = second_event.send(payload.clone()); });
        let worker = workers.lock().unwrap()[0].clone();
        worker.emit_to(LANE, json!({"target":1}), "one"); worker.emit(LANE, json!({"shared":2}));
        assert_eq!(first_events.recv().await.unwrap(), json!({"target":1}));
        assert_eq!(first_events.recv().await.unwrap(), json!({"shared":2}));
        assert_eq!(second_events.recv().await.unwrap(), json!({"shared":2}));
        first.close(); second.close(); stops.recv().await.unwrap(); shutdown.abort(None);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(runtime, scenario) }).await.unwrap(); result.unwrap();
    assert!(!socket.exists());
}
