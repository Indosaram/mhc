use std::{
    collections::{HashMap, HashSet, VecDeque},
    process::Stdio,
    time::Duration,
};

pub struct TerminateProcessTreesOptions {
    pub grace_ms: u64,
    pub kill_wait_ms: Option<u64>,
    pub owner_pid: Option<u32>,
}

pub async fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // PERF-DIVERGENCE(senpi process-tree): upstream polls via process.kill(pid, 0); this port uses a rustix kill(pid,0) syscall instead of spawning `kill -0`. Same answer as upstream: success or EPERM (the process exists but belongs to another user) is alive, ESRCH is not.
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
    // PERF-DIVERGENCE(senpi process-tree): Windows has no kill(pid, 0); upstream's
    // process.kill(pid, 0) opens the process handle, and this port does the same Win32 probe
    // instead of spawning `tasklist.exe` for every poll tick.
    #[cfg(windows)]
    {
        maho_agent::harness::env::windows_process_probe::is_process_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
fn unix_signal(signal: &str) -> Option<rustix::process::Signal> {
    use rustix::process::Signal;
    Some(match signal.strip_prefix('-')? {
        "HUP" | "1" => Signal::HUP,
        "INT" | "2" => Signal::INT,
        "QUIT" | "3" => Signal::QUIT,
        "KILL" | "9" => Signal::KILL,
        "TERM" | "15" => Signal::TERM,
        _ => return None,
    })
}

pub async fn signal_process(pid: u32, signal: &str) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    if let Some(sig) = unix_signal(signal) {
        return i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| rustix::process::kill_process(pid, sig).is_ok());
    }
    tokio::process::Command::new("kill")
        .args([signal, "--", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

async fn ps(args: &[&str]) -> String {
    bounded_process_output(
        tokio::process::Command::new("ps").args(args),
        16 * 1024 * 1024,
    )
    .await
}

async fn bounded_process_output(command: &mut tokio::process::Command, max_bytes: u64) -> String {
    use tokio::io::AsyncReadExt;
    let Ok(mut child) = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return String::new();
    };
    let Some(stdout) = child.stdout.take() else {
        return String::new();
    };
    let mut output = Vec::new();
    let mut limited = AsyncReadExt::take(stdout, max_bytes + 1);
    let read = limited.read_to_end(&mut output);
    if tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .is_err()
    {
        let _ = child.start_kill();
        return String::new();
    }
    if output.len() as u64 > max_bytes {
        output.truncate(max_bytes as usize);
        if let Err(error) = child.start_kill() {
            maho_ext_api::diagnostic(&format!("process table capture retirement failed: {error}"));
        }
    }
    if let Err(error) = child.wait().await {
        maho_ext_api::diagnostic(&format!("process table capture wait failed: {error}"));
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub async fn read_process_table() -> HashMap<u32, Vec<u32>> {
    let mut table: HashMap<u32, Vec<u32>> = HashMap::new();
    if cfg!(windows) {
        return table;
    }
    for line in ps(&["-axo", "pid=,ppid="]).await.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(Ok(pid)), Some(Ok(parent))) =
            (fields.next().map(str::parse), fields.next().map(str::parse))
        {
            table.entry(parent).or_default().push(pid);
        }
    }
    table
}

pub fn collect_descendants(table: &HashMap<u32, Vec<u32>>, roots: &[u32]) -> Vec<u32> {
    let mut seen: HashSet<_> = roots.iter().copied().collect();
    let mut queue: VecDeque<_> = roots.iter().copied().collect();
    let mut descendants = Vec::new();
    while let Some(parent) = queue.pop_front() {
        for child in table.get(&parent).into_iter().flatten() {
            if seen.insert(*child) {
                descendants.push(*child);
                queue.push_back(*child);
            }
        }
    }
    descendants
}

pub fn owned_roots(table: &HashMap<u32, Vec<u32>>, parent_pid: u32, roots: &[u32]) -> Vec<u32> {
    roots
        .iter()
        .copied()
        .filter(|root| {
            table
                .get(&parent_pid)
                .is_some_and(|children| children.contains(root))
        })
        .collect()
}

async fn without_zombies(pids: Vec<u32>) -> Vec<u32> {
    if pids.is_empty() || cfg!(windows) {
        return pids;
    }
    let ids = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut zombies = HashSet::new();
    for line in ps(&["-o", "pid=,stat=", "-p", &ids]).await.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(Ok(pid)), Some(state)) = (fields.next().map(str::parse::<u32>), fields.next())
            && state.starts_with('Z')
        {
            zombies.insert(pid);
        }
    }
    pids.into_iter()
        .filter(|pid| !zombies.contains(pid))
        .collect()
}

async fn wait_for_exit(pids: &[u32], grace_ms: u64) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(grace_ms);
    let mut remaining = Vec::new();
    for pid in pids {
        if is_process_alive(*pid).await {
            remaining.push(*pid);
        }
    }
    remaining = without_zombies(remaining).await;
    let mut zombie_check = tokio::time::Instant::now() + Duration::from_millis(250);
    while !remaining.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let mut live = Vec::new();
        for pid in remaining {
            if is_process_alive(pid).await {
                live.push(pid);
            }
        }
        remaining = live;
        if !remaining.is_empty() && tokio::time::Instant::now() >= zombie_check {
            remaining = without_zombies(remaining).await;
            zombie_check = tokio::time::Instant::now() + Duration::from_millis(250);
        }
    }
    remaining
}

pub async fn terminate_process_trees(roots: &[u32], options: TerminateProcessTreesOptions) {
    let mut live = Vec::new();
    for pid in roots {
        if is_process_alive(*pid).await {
            live.push(*pid);
        }
    }
    if live.is_empty() {
        return;
    }
    if cfg!(windows) {
        let rows = maho_agent::harness::env::windows_process_probe::process_rows();
        for root in live {
            let plan = maho_agent::harness::env::windows_process_probe::tree_kill_plan(
                &rows,
                root,
                std::process::id(),
            );
            if plan.kill.is_empty() {
                continue;
            }
            maho_agent::harness::env::windows_process_probe::terminate_all(&plan.kill);
            let remaining: Vec<u32> = plan
                .kill
                .iter()
                .copied()
                .filter(|pid| {
                    *pid > 1
                        && maho_agent::harness::env::windows_process_probe::is_process_alive(*pid)
                })
                .collect();
            if remaining.is_empty() {
                continue;
            }
            let mut args = vec!["/F".to_owned()];
            for pid in remaining {
                args.extend(["/PID".to_owned(), pid.to_string()]);
            }
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::process::Command::new("taskkill")
                    .args(&args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            )
            .await;
        }
        return;
    }
    let table = read_process_table().await;
    let roots = options
        .owner_pid
        .map_or_else(|| live.clone(), |owner| owned_roots(&table, owner, &live));
    if roots.is_empty() {
        return;
    }
    let descendants = collect_descendants(&table, &roots);
    let targets: Vec<_> = roots.into_iter().chain(descendants).collect();
    for pid in &targets {
        signal_process(*pid, "-TERM").await;
    }
    let survivors = wait_for_exit(&targets, options.grace_ms).await;
    for pid in &survivors {
        signal_process(*pid, "-KILL").await;
    }
    if !survivors.is_empty() {
        wait_for_exit(&survivors, options.kill_wait_ms.unwrap_or(options.grace_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn process_output_is_bounded_and_preserves_nonzero_partial_output() {
        let mut command = if cfg!(windows) {
            let mut cmd = tokio::process::Command::new("node");
            cmd.args(["-e", "process.stdout.write('123456789')"]);
            cmd
        } else {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.args(["-c", "printf 123456789"]);
            cmd
        };
        assert_eq!(bounded_process_output(&mut command, 8).await, "12345678");
        let mut command = if cfg!(windows) {
            let mut cmd = tokio::process::Command::new("node");
            cmd.args(["-e", "process.stdout.write('42 1'); process.exit(1)"]);
            cmd
        } else {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.args(["-c", "printf '42 1'; exit 1"]);
            cmd
        };
        assert_eq!(bounded_process_output(&mut command, 8).await, "42 1");
    }
}
