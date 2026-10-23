use std::sync::mpsc;
use std::time::Duration;

use super::{
    is_recall_term_save_excluded, track_recall_term_save, with_recall_term_saves_excluded,
};

#[test]
fn work_runs_excluded_and_the_exclusion_nests_and_lifts() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!is_recall_term_save_excluded(dir.path()));
    with_recall_term_saves_excluded(dir.path(), || {
        assert!(is_recall_term_save_excluded(dir.path()));
        with_recall_term_saves_excluded(dir.path(), || {
            assert!(is_recall_term_save_excluded(dir.path()));
        });
        assert!(is_recall_term_save_excluded(dir.path()));
    });
    assert!(!is_recall_term_save_excluded(dir.path()));
}

#[test]
fn work_waits_for_an_in_flight_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let guard = track_recall_term_save(dir.path());
    let (started, started_rx) = mpsc::channel();
    let (ran, ran_rx) = mpsc::channel();
    let path = dir.path().to_path_buf();
    let worker = std::thread::spawn(move || {
        started.send(()).expect("started");
        with_recall_term_saves_excluded(&path, || ran.send(()).expect("ran"));
    });
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker started");
    // While the save is registered the work cannot have run.
    assert!(ran_rx.recv_timeout(Duration::from_millis(0)).is_err());
    drop(guard);
    ran_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("work ran after the save finished");
    worker.join().expect("worker joined");
}
