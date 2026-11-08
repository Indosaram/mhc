#[cfg(unix)]
use std::collections::BTreeSet;
use std::time::Duration;
#[cfg(unix)]
use tokio::process::Command;

#[cfg(unix)]
pub async fn collect_process_tree(root: u32) -> Vec<u32> {
    let mut seen = BTreeSet::new();
    let mut queue = vec![root];
    let mut index = 0;
    while index < queue.len() {
        let pid = queue[index];
        index += 1;
        if !seen.insert(pid) {
            continue;
        }
        let output = tokio::time::timeout(
            Duration::from_secs(1),
            Command::new("pgrep")
                .args(["-P", &pid.to_string(), "."])
                .kill_on_drop(true)
                .output(),
        )
        .await;
        if let Ok(Ok(output)) = output
            && output.status.success()
        {
            for child in String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .filter(|pid| *pid > 0)
            {
                if !seen.contains(&child) {
                    queue.push(child);
                }
            }
        }
    }
    queue.into_iter().fold(Vec::new(), |mut result, pid| {
        if !result.contains(&pid) {
            result.push(pid);
        }
        result
    })
}

#[cfg(windows)]
pub async fn collect_process_tree(root: u32) -> Vec<u32> {
    maho_agent::harness::env::windows_process_probe::process_tree(root)
}

#[cfg(not(any(unix, windows)))]
pub async fn collect_process_tree(root: u32) -> Vec<u32> {
    vec![root]
}

pub async fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| match rustix::process::test_kill_process(pid) {
                Ok(()) => true,
                Err(error) => error == rustix::io::Errno::PERM,
            })
    }
    #[cfg(windows)]
    {
        maho_agent::harness::env::windows_process_probe::is_process_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

async fn kill_pids(pids: &[u32], signal: &str) {
    #[cfg(unix)]
    {
        let signal = if signal == "-KILL" {
            rustix::process::Signal::KILL
        } else {
            rustix::process::Signal::TERM
        };
        for pid in pids.iter().rev().filter(|pid| **pid > 1).filter_map(|pid| {
            i32::try_from(*pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        }) {
            let _ = rustix::process::kill_process(pid, signal);
        }
    }
    #[cfg(windows)]
    {
        let _ = signal;
        for pid in pids.iter().rev().filter(|pid| **pid > 1) {
            let _ = maho_agent::harness::env::windows_process_probe::terminate_process(*pid);
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pids, signal);
    }
}

async fn wait_for_dead(pids: &[u32], timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let mut alive = false;
        for pid in pids {
            if is_process_alive(*pid).await {
                alive = true;
                break;
            }
        }
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn reap_process_tree(root: u32, term_wait: Duration, kill_wait: Duration) {
    #[cfg(windows)]
    {
        let _ = term_wait;
        let rows = maho_agent::harness::env::windows_process_probe::process_rows();
        let plan = maho_agent::harness::env::windows_process_probe::tree_kill_plan(
            &rows,
            root,
            std::process::id(),
        );
        // Only the planned pids may be killed. Re-deriving the tree here would bypass the
        // protected-image and self-ancestor refusals the plan just decided.
        let pids = plan.kill.clone();
        if pids.is_empty() {
            return;
        }
        kill_pids(&pids, "").await;
        wait_for_dead(&pids, kill_wait).await;
        let mut remaining = Vec::new();
        for pid in &pids {
            if is_process_alive(*pid).await {
                remaining.push(*pid);
            }
        }
        if !remaining.is_empty() {
            kill_pids(&remaining, "").await;
            wait_for_dead(&remaining, kill_wait).await;
        }
    }
    #[cfg(not(windows))]
    {
        let mut known = collect_process_tree(root).await;
        let mut alive = false;
        for pid in &known {
            if is_process_alive(*pid).await {
                alive = true;
                break;
            }
        }
        if !alive {
            return;
        }
        kill_pids(&known, "-TERM").await;
        wait_for_dead(&known, term_wait).await;
        for pid in collect_process_tree(root).await {
            if !known.contains(&pid) {
                known.push(pid);
            }
        }
        let mut alive = false;
        for pid in &known {
            if is_process_alive(*pid).await {
                alive = true;
                break;
            }
        }
        if !alive {
            return;
        }
        kill_pids(&known, "-KILL").await;
        wait_for_dead(&known, kill_wait).await;
    }
}
