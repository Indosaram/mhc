pub mod supervisor_finished_child;
pub mod facts_child;
pub mod hold_lock;
pub mod supervisor_parent;
pub mod reflection_child;
pub mod supervisor_child;
pub mod supervisor_taskkill;
pub mod dream_child;
pub mod supervisor_keepalive;
pub mod supervisor_incomplete_outcome_then_fail;
pub mod supervisor_outcome_then_fail;
pub mod supervisor_outcome_then_linger;
pub mod supervisor_never_publishes;

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use crate::worker::run_artifacts::{RunLaunchManifest, RunKind, RunOutcome, read_run_json, write_run_json_atomic};
    fn seed(root: &std::path::Path) {
        write_run_json_atomic(&root.join("launch.json"), &RunLaunchManifest {
            version: 1, run_id: "fixture".into(), attempt: 2, next_attempt: None, kind: RunKind::Facts,
            command: "unused".into(), args: vec![], cwd: root.to_string_lossy().into_owned(), env: Default::default(),
            hard_deadline_at: 100.0, termination_grace_ms: 10.0, max_output_bytes: 1024,
            stdout_path: "stdout".into(), stderr_path: "stderr".into(),
        }, 0o600).unwrap();
    }
    #[test]
    fn incomplete_failure_preserves_launch() {
        let root = tempfile::tempdir().unwrap(); seed(root.path());
        assert_eq!(supervisor_incomplete_outcome_then_fail::run(root.path()).unwrap(), 1);
        let outcome: RunOutcome = read_run_json(&root.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.attempt, Some(2)); assert_eq!(outcome.child_exit.code, Some(0));
        assert!(root.path().join("launch.json").exists());
    }
    #[test]
    fn complete_failure_removes_launch_and_records_timeout() {
        let root = tempfile::tempdir().unwrap(); seed(root.path());
        assert_eq!(supervisor_outcome_then_fail::run(root.path()).unwrap(), 1);
        let outcome: RunOutcome = read_run_json(&root.path().join("outcome.json")).unwrap();
        assert!(outcome.timed_out); assert_eq!(outcome.child_exit.signal.as_deref(), Some("SIGTERM"));
        assert!(!root.path().join("launch.json").exists());
    }
    #[tokio::test]
    async fn linger_publishes_before_release_without_losing_attempt() {
        let root = tempfile::tempdir().unwrap(); seed(root.path());
        let child = supervisor_outcome_then_linger::run(root.path());
        tokio::pin!(child);
        assert!(std::future::poll_fn(|context| std::task::Poll::Ready(child.as_mut().poll(context).is_pending())).await);
        let outcome: RunOutcome = read_run_json(&root.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.attempt, Some(2)); assert_eq!(outcome.child_exit.code, Some(0));
        assert!(!root.path().join("released.json").exists());
        std::fs::write(root.path().join("release"), "").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), child).await.unwrap().unwrap();
        assert!(root.path().join("released.json").exists());
    }
    #[tokio::test]
    async fn never_publishes_only_records_release() {
        let root = tempfile::tempdir().unwrap();
        let child = supervisor_never_publishes::run(root.path());
        tokio::pin!(child);
        assert!(std::future::poll_fn(|context| std::task::Poll::Ready(child.as_mut().poll(context).is_pending())).await);
        assert!(!root.path().join("released.json").exists());
        std::fs::write(root.path().join("release"), "").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), child).await.unwrap().unwrap();
        assert!(!root.path().join("outcome.json").exists());
        assert!(root.path().join("released.json").exists());
    }
    #[test]
    fn finished_child_writes_machine_consumed_marker() {
        let root = tempfile::tempdir().unwrap(); supervisor_finished_child::run(root.path()).unwrap();
        let marker: serde_json::Value = read_run_json(&root.path().join("child-finished.json")).unwrap();
        assert_eq!(marker["finished"], true);
    }
}
