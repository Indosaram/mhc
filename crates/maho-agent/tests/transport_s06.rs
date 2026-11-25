//! Port of senpi `packages/agent/test/testing/win32-sim.test.ts` and
//! `packages/agent/test/transport/{lane-order-resume,transport-conformance,win32-sim}.test.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::durable::testing::manual_timers::create_manual_timers;
use maho_agent::testing::win32_sim::{
    Win32ErrorCode, Win32FsError, Win32OpenFileErrorCode, Win32SimFs, Win32SimFsOptions,
    to_win32_path, win32_error, win32_names_equal, win32_pipe_endpoint,
};
use maho_agent::transport::in_process_carrier::{
    InProcessNetworkOptions, create_in_process_network,
};
use maho_agent::transport::local_endpoint::{endpoint_key, local_endpoint};
use maho_agent::transport::reliable_transport::{
    ReliableTransportOptions, create_reliable_transport,
};
use maho_agent::transport::transport::{Transport, TransportClock};

use crate::s06_adapters::{DEFAULT_T0, reference_transport_adapter};

#[tokio::test]
async fn win32_sim_enforces_open_file_unlink_and_exec_mode_rules() {
    let err = win32_error(
        Win32ErrorCode::Ebusy,
        "unlink",
        Some("C:\\work\\file.txt"),
        None,
    );
    assert_eq!(err.code.as_deref(), Some("EBUSY"));
    assert_eq!(err.errno, Some(-4082));
    assert!(win32_names_equal("FOO.TXT", "foo.txt"));
    assert_eq!(
        to_win32_path("dir/file.txt", "C").expect("win32 path"),
        "C:\\dir\\file.txt"
    );
    assert!(win32_pipe_endpoint("senpi-test").is_ok());

    let dir = tempfile::tempdir().expect("tempdir");
    let fs = Win32SimFs::new(Win32SimFsOptions {
        root: dir.path().to_path_buf(),
        rename_open_file_error: Win32OpenFileErrorCode::Ebusy,
        delete_open_file_error: Win32OpenFileErrorCode::Eperm,
    });
    fs.mkdir("C:\\work").await.expect("mkdir");
    fs.write_file("C:\\work\\locked.txt", b"hi")
        .await
        .expect("write");

    let mut handle = fs
        .open_read("C:\\work\\locked.txt")
        .await
        .expect("open read");
    let unlink_err = fs
        .unlink("C:\\work\\locked.txt")
        .await
        .expect_err("open file cannot be unlinked in win32-sim");
    assert_eq!(unlink_err.win32_code(), Some("EPERM"));
    assert!(matches!(unlink_err, Win32FsError::Win32(_)));
    handle.close().await.expect("close handle");
    fs.unlink("C:\\work\\locked.txt")
        .await
        .expect("closed file unlinks cleanly");
}

#[tokio::test]
async fn transport_lane_order_resume_delivers_once_per_lane_and_resumes_on_reconnect() {
    let timers = create_manual_timers(DEFAULT_T0);
    let clock: Arc<dyn TransportClock> = Arc::new(timers.clone());
    let adapter = reference_transport_adapter();
    let left = adapter.create_side(clock.clone());
    let right = adapter.create_side(clock);

    let received = Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let rx_notify = Arc::new(tokio::sync::Notify::new());
    let r_clone = received.clone();
    let n_clone = rx_notify.clone();
    right.on_frame(Arc::new(move |frame| {
        let r_clone = r_clone.clone();
        let n_clone = n_clone.clone();
        Box::pin(async move {
            r_clone
                .lock()
                .expect("received")
                .push((frame.lane, frame.payload));
            n_clone.notify_waiters();
        })
    }));

    let timers_for_net = timers.clone();
    let network = create_in_process_network(InProcessNetworkOptions {
        schedule: Some(Arc::new(move |deliver| {
            let _ = timers_for_net.set_timer_sync(deliver, 0);
        })),
    });
    let ep = local_endpoint("linux", "lane-order", "/tmp").expect("endpoint");
    let _unlisten = network
        .listen(&ep, right.as_carrier_side())
        .expect("listen");
    let conn = network.dial(&ep, left.as_carrier_side()).expect("dial");
    conn.connect();

    let left_s1 = left.clone();
    let left_s2 = left.clone();
    let left_s3 = left.clone();
    let s1 = tokio::spawn(async move { left_s1.send("a", b"one".to_vec()).await });
    let s2 = tokio::spawn(async move { left_s2.send("a", b"two".to_vec()).await });
    let s3 = tokio::spawn(async move { left_s3.send("b", b"b1".to_vec()).await });
    tokio::task::yield_now().await;
    for step in 1..=10 {
        timers
            .advance_to(DEFAULT_T0 + step * 5)
            .await
            .expect("advance");
    }
    let (r1, r2, r3) = tokio::try_join!(s1, s2, s3).expect("join");
    r1.and(r2).and(r3).expect("all sends acked");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notified = rx_notify.notified();
            if received.lock().expect("received").len() >= 3 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("first batch delivered");
    conn.drop_link();

    // Reconnect the link and send again without losing or duplicating earlier payloads.
    conn.connect();
    let left_s4 = left.clone();
    let s4 = tokio::spawn(async move { left_s4.send("a", b"three".to_vec()).await });
    tokio::task::yield_now().await;
    for step in 11..=20 {
        timers
            .advance_to(DEFAULT_T0 + step * 5)
            .await
            .expect("advance");
    }
    s4.await.expect("join").expect("resumed send acked");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notified = rx_notify.notified();
            if received.lock().expect("received").len() >= 4 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("resumed frame delivered");
    conn.drop_link();
    left.close();
    right.close();

    let got = received.lock().expect("received").clone();
    assert_eq!(
        got.iter()
            .filter(|(l, _)| l == "a")
            .map(|(_, p)| p.as_slice())
            .collect::<Vec<_>>(),
        vec![b"one".as_slice(), b"two".as_slice(), b"three".as_slice()]
    );
    assert_eq!(
        got.iter()
            .filter(|(l, _)| l == "b")
            .map(|(_, p)| p.as_slice())
            .collect::<Vec<_>>(),
        vec![b"b1".as_slice()]
    );
}

#[tokio::test]
async fn local_endpoint_pair_connects_and_delivers_frames_without_timers() {
    let ep_a = local_endpoint("linux", "pair-one", "/run/senpi").expect("ep_a");
    let ep_b = local_endpoint("win32", "PAIR-ONE", "/run/senpi").expect("ep_b");
    let ep_b_lower = local_endpoint("win32", "pair-one", "/run/senpi").expect("ep_b_lower");
    assert_eq!(ep_a.address(), "/run/senpi/pair-one.sock");
    assert_eq!(endpoint_key(&ep_b), endpoint_key(&ep_b_lower));

    let timers = create_manual_timers(DEFAULT_T0);
    let rt = create_reliable_transport(ReliableTransportOptions {
        clock: Arc::new(timers),
        window: Some(4),
        retransmit_ms: Some(100),
        on_handler_error: Arc::new(|_, _| {}),
    });
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();
    rt.on_frame(Arc::new(move |_frame| {
        let count_clone = count_clone.clone();
        Box::pin(async move {
            count_clone.fetch_add(1, Ordering::SeqCst);
        })
    }));
    rt.close();
}
