//! Cleanup for launches whose server went away (senpi#3566 todo 4; review HIGH 2); port of `orphan-sweep.ts`. The
//! ledger is fsynced before every ack, so it names every owned group even when its writer is dead: a child that loses
//! its host sweeps them before it exits, and a server starting a session sweeps whatever a crashed predecessor left
//! before it writes a new ledger. Detached launches are never touched; the identity check in `sweep_groups` keeps a
//! reused pgid safe.

use std::io;
use std::path::Path;

use crate::descendants::{DescendantsLedger, generation_ledger_path};
use crate::group_ownership::{
    LedgeredGroup, SweepOptions, SweepOutcome, default_group_probes, process_is_live, sweep_groups,
};

const SWEEP_TIMING: SweepOptions = SweepOptions {
    grace_ms: 500,
    kill_wait_ms: 1_000,
    poll_ms: None,
};

/// Sweeps the owned groups one ledger names.
pub async fn sweep_ledger(path: &Path) -> Vec<SweepOutcome> {
    let groups: Vec<LedgeredGroup> = DescendantsLedger::read_persisted(path)
        .await
        .into_iter()
        .filter_map(|entry| {
            entry.pgid.map(|pgid| LedgeredGroup {
                launch_id: entry.launch_id,
                pgid,
                leader_start: entry.leader_start,
                lifetime: entry.lifetime.into(),
            })
        })
        .collect();
    if groups.is_empty() {
        return Vec::new();
    }
    sweep_groups(&groups, SWEEP_TIMING, &default_group_probes()).await
}

/// Sweeps and removes every generation ledger left in a session's tool-child directory.
/// The file in a generation's directory naming the pid of the child that generation started.
pub const GENERATION_CHILD_FILE: &str = "child.pid";

/// Records the child a generation started, so a later start can tell a live generation from a stale one.
pub async fn record_generation_child(dir: &Path, generation: u64, pid: u32) -> io::Result<()> {
    let ledger = generation_ledger_path(dir, generation);
    let generation_dir = ledger.parent().unwrap_or(dir);
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(generation_dir).await?;
    let file = generation_dir.join(GENERATION_CHILD_FILE);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut handle = options.open(file).await?;
    tokio::io::AsyncWriteExt::write_all(&mut handle, pid.to_string().as_bytes()).await?;
    tokio::io::AsyncWriteExt::flush(&mut handle).await
}

async fn child_alive(file: &Path) -> bool {
    let text = tokio::fs::read_to_string(file).await.unwrap_or_default();
    let pid = match text.trim() {
        "" => 0,
        trimmed => trimmed.parse::<u64>().unwrap_or(0),
    };
    // `Number.isSafeInteger(pid) && pid > 1`; a pid beyond u32 cannot name a process.
    if pid <= 1 || pid > (1 << 53) - 1 {
        return false;
    }
    u32::try_from(pid).is_ok_and(process_is_live)
}

pub async fn sweep_stale_generations(dir: &Path) -> Vec<SweepOutcome> {
    let mut names: Vec<String> = match tokio::fs::read_dir(dir).await {
        Ok(mut entries) => {
            let mut names = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_owned());
                }
            }
            names
        }
        Err(_) => Vec::new(),
    };
    names.sort();
    let mut outcomes = Vec::new();
    for name in names {
        let Some(generation) = name
            .strip_prefix('g')
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok())
        else {
            continue;
        };
        // A generation whose child still runs is live - another host process on the same session - and is not ours to sweep.
        if child_alive(&dir.join(&name).join(GENERATION_CHILD_FILE)).await {
            continue;
        }
        outcomes.extend(sweep_ledger(&generation_ledger_path(dir, generation)).await);
        let _ = tokio::fs::remove_dir_all(dir.join(&name)).await;
    }
    outcomes
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::group_ownership::{GroupVerdict, default_group_probes};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Kills the recorded pids' groups when the test ends, whatever its outcome.
    struct KillLater(Vec<u32>);

    impl Drop for KillLater {
        fn drop(&mut self) {
            let probes = default_group_probes();
            for pid in &self.0 {
                (probes.signal_group)(i64::from(*pid), crate::group_ownership::GroupSignal::Kill);
                if let Ok(raw) = i32::try_from(*pid)
                    && let Some(pid) = rustix::process::Pid::from_raw(raw)
                {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
        }
    }

    fn alive(pid: u32) -> bool {
        // A zombie still answers signal 0, so each spawned child is reaped by its own thread (see `spawn_sleep`).
        process_is_live(pid)
    }

    /// Spawns `/bin/sleep 300`; the returned channel fires when the process has been reaped (its exit is an event).
    fn spawn_sleep(detached: bool) -> (u32, std::sync::mpsc::Receiver<()>) {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if detached {
            command.process_group(0);
        }
        let mut child = command.spawn().expect("spawn the orphan fixture");
        let pid = child.id();
        let (reaped_tx, reaped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = reaped_tx.send(());
        });
        (pid, reaped)
    }

    fn entry_line(pid: u32, generation: u64, lifetime: &str, leader_start: &str) -> String {
        serde_json::json!({
            "launchId": format!("W1-{pid}"),
            "generation": generation,
            "origin": "tool",
            "api": "cp.spawn",
            "containment": "own-group",
            "lifetime": lifetime,
            "pid": pid,
            "pgid": pid,
            "atMs": 1,
            "leaderStart": leader_start,
        })
        .to_string()
    }

    async fn start_of(pid: u32) -> String {
        (default_group_probes().start_of)(i64::from(pid))
            .await
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn sweeps_the_owned_groups_a_crashed_predecessors_ledger_names_and_removes_that_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let (owned_pid, owned_reaped) = spawn_sleep(true);
        let (detached_pid, detached_reaped) = spawn_sleep(true);
        let _cleanup = KillLater(vec![owned_pid, detached_pid]);
        let ledger = generation_ledger_path(dir.path(), 1);
        tokio::fs::create_dir_all(ledger.parent().unwrap())
            .await
            .unwrap();
        let text = format!(
            "{}\n{}\n",
            entry_line(owned_pid, 1, "owned", &start_of(owned_pid).await),
            entry_line(detached_pid, 1, "detached", &start_of(detached_pid).await),
        );
        tokio::fs::write(&ledger, text).await.unwrap();
        let outcomes = sweep_stale_generations(dir.path()).await;
        let shape: Vec<(i64, GroupVerdict)> =
            outcomes.iter().map(|o| (o.pgid, o.verdict)).collect();
        assert_eq!(
            shape,
            vec![
                (i64::from(owned_pid), GroupVerdict::Sweep),
                (i64::from(detached_pid), GroupVerdict::Detached)
            ]
        );
        tokio::task::spawn_blocking(move || owned_reaped.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        assert!(!alive(owned_pid));
        assert!(alive(detached_pid));
        assert!(detached_reaped.try_recv().is_err());
        assert!(!ledger.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn leaves_a_generation_whose_child_still_runs_another_host_on_the_same_session_untouched()
    {
        let dir = tempfile::tempdir().unwrap();
        let (live_child, _live_reaped) = spawn_sleep(false);
        let (owned_pid, _owned_reaped) = spawn_sleep(true);
        let _cleanup = KillLater(vec![live_child, owned_pid]);
        let ledger = generation_ledger_path(dir.path(), 4);
        record_generation_child(dir.path(), 4, live_child)
            .await
            .unwrap();
        tokio::fs::write(
            &ledger,
            format!(
                "{}\n",
                entry_line(owned_pid, 4, "owned", &start_of(owned_pid).await)
            ),
        )
        .await
        .unwrap();
        assert_eq!(sweep_stale_generations(dir.path()).await, Vec::new());
        assert!(alive(owned_pid));
        assert!(ledger.exists());
    }
}
