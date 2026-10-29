use std::sync::{Arc, Mutex};

use super::{schedule_dag_retention_sweep, DagRetentionSweepLogger, DagRetentionSweepOptions};

#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl DagRetentionSweepLogger for Log {
    fn info(&self, message: &str) { self.0.lock().expect("log mutex").push(format!("info:{message}")); }
    fn warn(&self, message: &str) { self.0.lock().expect("log mutex").push(format!("warn:{message}")); }
}

#[test]
fn given_expired_run_when_sweep_prunes_then_reclaimed_count_is_logged() {
    let root = std::env::temp_dir().join(format!("dag-retention-logged-{}", std::process::id()));
    let store = Arc::new(senpi_task::dag::store::create_dag_file_store(&senpi_task::dag::store::DagStoreConfig::new(&root), Default::default()).expect("create store"));
    let run_id = "expired-log";
    let old = "2000-01-01T00:00:00.000Z";
    store.write_checkpoint(run_id, &serde_json::json!({"schemaVersion":1,"runId":run_id,"parentSessionId":"p","status":"completed","completedAt":old,"updatedAt":old,"nodes":[]})).expect("seed terminal run");
    let logger = Arc::new(Log::default());
    schedule_dag_retention_sweep(store, DagRetentionSweepOptions { schedule:Some(Arc::new(|sweep| sweep())), logger:logger.clone() });
    assert_eq!(logger.0.lock().expect("log mutex").as_slice(), ["info:DAG retention reclaimed 1 expired run(s)"]);
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn given_prune_failure_when_sweep_runs_then_failure_is_warned_without_propagation() {
    let root = std::env::temp_dir().join(format!("dag-retention-failure-{}", std::process::id()));
    let store = Arc::new(senpi_task::dag::store::create_dag_file_store(&senpi_task::dag::store::DagStoreConfig::new(&root), Default::default()).expect("create store"));
    std::fs::create_dir_all(&store.paths.runs).expect("create run directory");
    std::fs::remove_dir_all(&store.paths.runs).expect("remove run directory");
    std::fs::write(&store.paths.runs, "not a directory").expect("replace run directory with file");
    let logger = Arc::new(Log::default());
    schedule_dag_retention_sweep(store, DagRetentionSweepOptions { schedule:Some(Arc::new(|sweep| sweep())), logger:logger.clone() });
    assert!(logger.0.lock().expect("log mutex").iter().any(|line| line.starts_with("warn:DAG retention sweep failed:")));
    std::fs::remove_dir_all(root.clone()).or_else(|_| std::fs::remove_file(root)).expect("remove fixture");
}

#[test]
fn given_injected_scheduler_when_scheduled_then_prune_runs_once_and_reports_reclaimed_count() {
    let root = std::env::temp_dir().join(format!("dag-retention-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Arc::new(senpi_task::dag::store::create_dag_file_store(
        &senpi_task::dag::store::DagStoreConfig::new(&root),
        senpi_task::dag::store::DagStoreOptions::default(),
    ).expect("create store"));
    let queued = Arc::new(Mutex::new(None));
    let scheduler_queue = queued.clone();
    let logger = Arc::new(Log::default());
    schedule_dag_retention_sweep(store, DagRetentionSweepOptions {
        schedule: Some(Arc::new(move |sweep| *scheduler_queue.lock().expect("scheduler mutex") = Some(sweep))),
        logger: logger.clone(),
    });
    assert!(logger.0.lock().expect("log mutex").is_empty());
    queued.lock().expect("scheduler mutex").take().expect("scheduled sweep")();
    assert!(logger.0.lock().expect("log mutex").is_empty());
    assert!(queued.lock().expect("scheduler mutex").is_none(), "one runtime schedules only one sweep closure");
    std::fs::remove_dir_all(root).or_else(|error| {
        if error.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(error) }
    }).expect("remove fixture");
}
