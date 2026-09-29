use super::install_process_signal_cleanup;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn terminate_signal_runs_cleanup_until_guard_is_dropped() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let guard = install_process_signal_cleanup(move || {
        let sender = sender.clone();
        async move {
            let _ = sender.send("cleanup");
        }
    })
    .expect("runtime present");

    // SAFETY: raise() only delivers SIGTERM to this test process, whose handler is installed.
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);

    let received = tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await;
    assert_eq!(received, Ok(Some("cleanup")));
    drop(guard);
    assert_eq!(receiver.recv().await, None);
}
