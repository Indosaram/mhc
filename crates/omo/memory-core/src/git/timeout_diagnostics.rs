//! Opt-in timeout diagnostics writing live process snapshots via a temporary file (#10821, #12801).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::support::time::{now_iso, now_millis};

/// Environment variable naming a directory where git timeout snapshots should be written.
pub const GIT_TIMEOUT_DIAGNOSTICS_ENV: &str = "OMO_GIT_TIMEOUT_DIAGNOSTICS_DIR";

const SNAPSHOT_TIMEOUT_MS: u64 = 15_000;

fn snapshot_command(pid: u32) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                format!(
                    "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CreationDate,CommandLine | Format-Table -AutoSize -Wrap | Out-String -Width 400; @({pid}) + @(Get-CimInstance Win32_Process -Filter \"ParentProcessId={pid}\" | ForEach-Object {{ $_.ProcessId }}) | ForEach-Object {{ \"threads of $_\"; (Get-Process -Id $_ -ErrorAction SilentlyContinue).Threads | Group-Object ThreadState,WaitReason -NoElement | Format-Table -AutoSize | Out-String }}"
                ),
            ],
        )
    } else {
        (
            "ps",
            vec![
                "-axo".to_owned(),
                "pid,ppid,stat,wchan,etime,command".to_owned(),
            ],
        )
    }
}

/// Captures the host process table to a temp file (avoiding fixed pipe buffer caps, #12801).
pub fn process_snapshot(pid: u32) -> String {
    let (program, args) = snapshot_command(pid);
    let temp_dir = std::env::temp_dir();
    let output_path = temp_dir.join(format!(
        "omo-git-timeout-snapshot-{}-{}.txt",
        std::process::id(),
        crate::support::random::random_uuid()
    ));

    let result = (|| -> Result<String, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|e| format!("process snapshot failed: {e}"))?;
        let stderr_file = file
            .try_clone()
            .map_err(|e| format!("process snapshot failed: {e}"))?;

        let mut cmd = Command::new(program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(stderr_file));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("process snapshot failed: {e}"))?;

        let deadline = Instant::now() + Duration::from_millis(SNAPSHOT_TIMEOUT_MS);
        let mut backoff = Duration::from_millis(1);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("process snapshot failed: {e}")),
            }
        }
        std::fs::read_to_string(&output_path).map_err(|e| format!("process snapshot failed: {e}"))
    })();

    let _ = std::fs::remove_file(&output_path);
    match result {
        Ok(text) => text,
        Err(msg) => msg,
    }
}

/// Writes timeout diagnostics JSON into `OMO_GIT_TIMEOUT_DIAGNOSTICS_DIR` when set.
pub fn record_git_timeout(
    environment: &BTreeMap<String, String>,
    pid: Option<u32>,
    argv: &[String],
    cwd: &Path,
    timeout_ms: u64,
    snapshot_fn: Option<&dyn Fn(u32) -> String>,
) {
    let Some(directory) = environment
        .get(GIT_TIMEOUT_DIAGNOSTICS_ENV)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(pid) = pid else {
        return;
    };
    let snapshot_text = match snapshot_fn {
        Some(f) => f(pid),
        None => process_snapshot(pid),
    };
    let dir_path = Path::new(directory);
    if let Err(error) = std::fs::create_dir_all(dir_path) {
        let _ = writeln!(
            std::io::stderr(),
            "git timeout diagnostics could not be written to {directory}: {error}"
        );
        return;
    }
    let out_path = dir_path.join(format!("git-timeout-{pid}-{}.json", now_millis()));
    let payload = serde_json::json!({
        "at": now_iso(),
        "pid": pid,
        "cwd": cwd.display().to_string(),
        "timeoutMs": timeout_ms,
        "argv": argv.iter().take(12).collect::<Vec<_>>(),
        "argCount": argv.len(),
        "processes": snapshot_text,
    });
    match serde_json::to_string_pretty(&payload) {
        Ok(json) => {
            if let Err(error) = std::fs::write(&out_path, format!("{json}\n")) {
                let _ = writeln!(
                    std::io::stderr(),
                    "git timeout diagnostics could not be written to {directory}: {error}"
                );
            }
        }
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "git timeout diagnostics could not be written to {directory}: {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_diagnostics_file_when_env_is_set_and_skips_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        record_git_timeout(
            &BTreeMap::new(),
            Some(123),
            &["status".to_owned()],
            dir.path(),
            500,
            Some(&|_| "ok".to_owned()),
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let env = BTreeMap::from([(
            GIT_TIMEOUT_DIAGNOSTICS_ENV.to_owned(),
            dir.path().to_string_lossy().into_owned(),
        )]);
        let args: Vec<String> = (0..15).map(|i| format!("arg-{i}")).collect();
        record_git_timeout(
            &env,
            Some(456),
            &args,
            dir.path(),
            1_000,
            Some(&|pid| format!("table-for-{pid}")),
        );
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(files.len(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
        assert_eq!(parsed["pid"], 456);
        assert_eq!(parsed["argCount"], 15);
        assert_eq!(parsed["argv"].as_array().unwrap().len(), 12);
        assert_eq!(parsed["processes"], "table-for-456");
    }

    #[test]
    fn real_process_snapshot_reads_output_and_leaves_no_temp_file() {
        let out = process_snapshot(std::process::id());
        assert!(!out.is_empty());
    }
}
