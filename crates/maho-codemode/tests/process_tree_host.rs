use maho_codemode::kernels::js::process_tree_host::*;
use std::collections::HashMap;

#[test]
fn descendants_preserve_breadth_order_and_ignore_cycles() {
    let table = HashMap::from([(1, vec![2, 3]), (2, vec![4]), (3, vec![4, 5]), (5, vec![1])]);
    assert_eq!(collect_descendants(&table, &[1]), [2, 3, 4, 5]);
    assert_eq!(owned_roots(&table, 1, &[2, 5, 3]), [2, 3]);
}

#[cfg(unix)]
#[tokio::test]
async fn owned_child_is_retired_and_unowned_root_is_not_signalled() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("3600")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    terminate_process_trees(
        &[pid],
        TerminateProcessTreesOptions {
            grace_ms: 100,
            kill_wait_ms: Some(100),
            owner_pid: Some(pid),
        },
    )
    .await;
    assert!(child.try_wait().unwrap().is_none());
    terminate_process_trees(
        &[pid],
        TerminateProcessTreesOptions {
            grace_ms: 100,
            kill_wait_ms: Some(100),
            owner_pid: Some(std::process::id()),
        },
    )
    .await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
}

/// senpi `isProcessAlive`: an EPERM probe means the process exists under another user, so it is alive.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn foreign_process_answering_eperm_counts_as_alive() {
    // PID 1 always exists; a non-root probe gets EPERM, a root probe gets success. Both are alive.
    assert!(maho_codemode::kernels::js::process_tree_host::is_process_alive(1).await);
    assert!(!maho_codemode::kernels::js::process_tree_host::is_process_alive(0).await);
}
