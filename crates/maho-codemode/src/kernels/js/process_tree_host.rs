use std::{collections::{HashMap, HashSet, VecDeque}, process::Stdio, time::Duration};

pub struct TerminateProcessTreesOptions {
    pub grace_ms: u64,
    pub kill_wait_ms: Option<u64>,
    pub owner_pid: Option<u32>,
}

pub async fn is_process_alive(pid: u32) -> bool {
    if pid == 0 { return false; }
    tokio::process::Command::new("kill").args(["-0", "--", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().await.is_ok_and(|status| status.success())
}

pub async fn signal_process(pid: u32, signal: &str) -> bool {
    if pid == 0 { return false; }
    tokio::process::Command::new("kill").args([signal, "--", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().await.is_ok_and(|status| status.success())
}

async fn ps(args: &[&str]) -> String {
    bounded_process_output(tokio::process::Command::new("ps").args(args),16*1024*1024).await
}

async fn bounded_process_output(command: &mut tokio::process::Command, max_bytes: u64) -> String {
    use tokio::io::AsyncReadExt;
    let Ok(mut child)=command.stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn() else {return String::new();};
    let Some(stdout)=child.stdout.take() else {return String::new();};
    let mut output=Vec::new();
    if let Err(error)=AsyncReadExt::take(stdout,max_bytes+1).read_to_end(&mut output).await {eprintln!("process table output read failed: {error}");}
    if output.len() as u64>max_bytes {
        output.truncate(max_bytes as usize);
        if let Err(error)=child.start_kill() {eprintln!("process table capture retirement failed: {error}");}
    }
    if let Err(error)=child.wait().await {eprintln!("process table capture wait failed: {error}");}
    String::from_utf8_lossy(&output).into_owned()
}

pub async fn read_process_table() -> HashMap<u32, Vec<u32>> {
    let mut table: HashMap<u32, Vec<u32>> = HashMap::new();
    if cfg!(windows) { return table; }
    for line in ps(&["-axo", "pid=,ppid="]).await.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(Ok(pid)), Some(Ok(parent))) = (fields.next().map(str::parse), fields.next().map(str::parse)) { table.entry(parent).or_default().push(pid); }
    }
    table
}

pub fn collect_descendants(table: &HashMap<u32, Vec<u32>>, roots: &[u32]) -> Vec<u32> {
    let mut seen: HashSet<_> = roots.iter().copied().collect();
    let mut queue: VecDeque<_> = roots.iter().copied().collect();
    let mut descendants = Vec::new();
    while let Some(parent) = queue.pop_front() {
        for child in table.get(&parent).into_iter().flatten() {
            if seen.insert(*child) { descendants.push(*child); queue.push_back(*child); }
        }
    }
    descendants
}

pub fn owned_roots(table: &HashMap<u32, Vec<u32>>, parent_pid: u32, roots: &[u32]) -> Vec<u32> {
    roots.iter().copied().filter(|root|table.get(&parent_pid).is_some_and(|children|children.contains(root))).collect()
}

async fn without_zombies(pids: Vec<u32>) -> Vec<u32> {
    if pids.is_empty() || cfg!(windows) { return pids; }
    let ids = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut zombies = HashSet::new();
    for line in ps(&["-o", "pid=,stat=", "-p", &ids]).await.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(Ok(pid)), Some(state)) = (fields.next().map(str::parse::<u32>), fields.next()) && state.starts_with('Z') { zombies.insert(pid); }
    }
    pids.into_iter().filter(|pid|!zombies.contains(pid)).collect()
}

async fn wait_for_exit(pids: &[u32], grace_ms: u64) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(grace_ms);
    let mut remaining = Vec::new();
    for pid in pids { if is_process_alive(*pid).await { remaining.push(*pid); } }
    remaining = without_zombies(remaining).await;
    let mut zombie_check = tokio::time::Instant::now() + Duration::from_millis(250);
    while !remaining.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let mut live = Vec::new();
        for pid in remaining { if is_process_alive(pid).await { live.push(pid); } }
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
    for pid in roots { if is_process_alive(*pid).await { live.push(*pid); } }
    if live.is_empty() { return; }
    if cfg!(windows) {
        for pid in live { let _ = tokio::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().await; }
        return;
    }
    let table = read_process_table().await;
    let roots = options.owner_pid.map_or_else(||live.clone(), |owner|owned_roots(&table, owner, &live));
    if roots.is_empty() { return; }
    let descendants = collect_descendants(&table, &roots);
    let targets: Vec<_> = roots.into_iter().chain(descendants).collect();
    for pid in &targets { signal_process(*pid, "-TERM").await; }
    let survivors = wait_for_exit(&targets, options.grace_ms).await;
    for pid in &survivors { signal_process(*pid, "-KILL").await; }
    if !survivors.is_empty() { wait_for_exit(&survivors, options.kill_wait_ms.unwrap_or(options.grace_ms)).await; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn process_output_is_bounded_and_preserves_nonzero_partial_output() {
        let mut command=tokio::process::Command::new("sh");
        command.args(["-c","printf 123456789"]);
        assert_eq!(bounded_process_output(&mut command,8).await,"12345678");
        let mut command=tokio::process::Command::new("sh");
        command.args(["-c","printf '42 1'; exit 1"]);
        assert_eq!(bounded_process_output(&mut command,8).await,"42 1");
    }
}
