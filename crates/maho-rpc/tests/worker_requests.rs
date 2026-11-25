use maho_rpc::session_worker_requests::*;
#[tokio::test(start_paused = true)]
async fn opening_timer_fires_without_retiring_request_debt() {
    let mut requests = SessionWorkerRequests::default();
    let (timeout, fired) = tokio::sync::oneshot::channel();
    let reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            0,
            |_| Ok(()),
            move || {
                let _ = timeout.send(());
            },
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(30_000)).await;
    fired.await.unwrap();
    assert_eq!(requests.active_count(), 1);
    requests.close_with_error("session_worker_request_timeout");
    assert_eq!(
        reply.await.unwrap(),
        Err("session_worker_request_timeout".into())
    );
}
#[tokio::test(start_paused = true)]
async fn reply_cancels_its_timer_before_the_deadline() {
    let mut requests = SessionWorkerRequests::default();
    let (timeout, fired) = tokio::sync::oneshot::channel();
    let reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            0,
            |_| Ok(()),
            move || {
                let _ = timeout.send(());
            },
        )
        .unwrap();
    requests.receive(&serde_json::json!({"type":"result","request":1}));
    assert!(reply.await.unwrap().is_ok());
    tokio::time::advance(std::time::Duration::from_millis(30_000)).await;
    assert!(fired.await.is_err());
    assert_eq!(requests.active_count(), 0);
}
#[tokio::test(start_paused = true)]
async fn failed_send_never_installs_a_timer() {
    let mut requests = SessionWorkerRequests::default();
    let (timeout, fired) = tokio::sync::oneshot::channel();
    let reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            0,
            |_| Err("closed".into()),
            move || {
                let _ = timeout.send(());
            },
        )
        .unwrap();
    assert_eq!(reply.await.unwrap(), Err("closed".into()));
    assert!(fired.await.is_err());
}
#[tokio::test]
async fn typed_prepared_reply_settles_registered_promise() {
    let mut requests = SessionWorkerRequests::default();
    let reply = requests
        .submit(
            &serde_json::json!({"type":"prepare","configuration":{},"profile":{}}),
            0,
            |_| Ok(()),
        )
        .unwrap();
    requests.receive_reply(
        &maho_rpc::session_worker_protocol::SessionWorkerToHost::Prepared {
            request: 1,
            session_path: "/session".into(),
        },
    );
    assert_eq!(
        reply.await.unwrap().unwrap(),
        serde_json::json!({"type":"prepared","request":1,"sessionPath":"/session"})
    );
    assert_eq!(requests.active_count(), 0);
    assert!(
        requests
            .receive_reply(
                &maho_rpc::session_worker_protocol::SessionWorkerToHost::Result {
                    request: 1,
                    error: None
                }
            )
            .is_none()
    );
}
#[tokio::test]
async fn channel_replies_and_close_settle_admitted_request_promises() {
    let mut requests = SessionWorkerRequests::default();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let reply = requests
        .submit(&serde_json::json!({"type":"commit"}), 0, |request| {
            send.send(request).map_err(|error| error.to_string())
        })
        .unwrap();
    let request = receive.recv().await.unwrap();
    requests.receive(
        &serde_json::json!({"type":"result","request":request["request"],"error":"denied"}),
    );
    assert_eq!(reply.await.unwrap(), Err("denied".into()));
    let reply = requests
        .submit(&serde_json::json!({"type":"bind"}), 1, |_| Ok(()))
        .unwrap();
    assert_eq!(requests.close_with_error("worker gone"), vec![2]);
    assert_eq!(reply.await.unwrap(), Err("worker gone".into()));
    assert_eq!(requests.active_count(), 0);
}
#[tokio::test]
async fn failed_send_rejects_its_promise_and_retires_debt() {
    let mut requests = SessionWorkerRequests::default();
    let reply = requests
        .submit(&serde_json::json!({"type":"commit"}), 0, |_| {
            Err("channel closed".into())
        })
        .unwrap();
    assert_eq!(reply.await.unwrap(), Err("channel closed".into()));
    assert_eq!(requests.active_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn does_not_start_open_already_expired_when_queue_built_long_ago() {
    let mut requests = SessionWorkerRequests::default();
    let now_late = maho_rpc::session_worker_protocol::SESSION_WORKER_LIMITS.open_ms + 5_000;
    tokio::time::advance(std::time::Duration::from_millis(now_late)).await;
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let _reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            now_late,
            |_| Ok(()),
            move || {
                let _ = tx.send(());
            },
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn does_not_start_request_already_expired_after_quiet_gap() {
    let mut requests = SessionWorkerRequests::default();
    let open_ms = maho_rpc::session_worker_protocol::SESSION_WORKER_LIMITS.open_ms;
    let (send_tx, mut send_rx) = tokio::sync::mpsc::unbounded_channel();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            0,
            move |msg| send_tx.send(msg).map_err(|e| e.to_string()),
            || {},
        )
        .unwrap();
    let sent = send_rx.recv().await.unwrap();
    requests.note_progress(0);
    requests.receive(&serde_json::json!({"type":"result","request":sent["request"]}));
    assert!(reply.await.unwrap().is_ok());

    let later = open_ms + 5_000;
    tokio::time::advance(std::time::Duration::from_millis(later)).await;
    let _reply2 = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"prompt_surface","surface":"app"}),
            later,
            |_| Ok(()),
            move || {
                let _ = tx.send(());
            },
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(rx.try_recv().is_err());
    tokio::time::advance(std::time::Duration::from_millis(open_ms)).await;
    rx.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn still_bounds_open_that_genuinely_stalls() {
    let mut requests = SessionWorkerRequests::default();
    let open_ms = maho_rpc::session_worker_protocol::SESSION_WORKER_LIMITS.open_ms;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _reply = requests
        .submit_with_timeout(
            &serde_json::json!({"type":"commit"}),
            0,
            |_| Ok(()),
            move || {
                let _ = tx.send(());
            },
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(open_ms + 1)).await;
    rx.await.unwrap();
}
