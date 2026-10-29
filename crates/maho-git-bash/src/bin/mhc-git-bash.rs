//! `mhc-git-bash`: the Git Bash command runner served over stdio MCP (`docs/cleanroom/specs/
//! git-bash-mcp.md`). `mhc-git-bash` or `mhc-git-bash mcp` serves on native Windows when Git Bash
//! is found and exits 0 at once otherwise; any other argument prints usage and exits 2.

use maho_git_bash::{Server, ServerOptions, SystemHost, run_cli};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let server = Server::new(SystemHost::default(), ServerOptions::default());
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let code = run_cli(&args, &server, stdin, stdout, &mut stderr, parent_exit()).await;
    // Exit directly: the stdin reader and the parent watch live on blocking threads that would
    // otherwise keep the runtime from shutting down.
    std::process::exit(code)
}

/// Resolves when the parent process has exited.
#[cfg(windows)]
async fn parent_exit() {
    match tokio::task::spawn_blocking(wait_for_parent).await {
        Ok(true) => {}
        // The parent could not be identified or watched: rely on stdin end of file alone.
        Ok(false) | Err(_) => std::future::pending::<()>().await,
    }
}

/// Blocks until the parent exits; `false` when the parent cannot be watched.
#[cfg(windows)]
fn wait_for_parent() -> bool {
    use winsafe::{HPROCESS, HPROCESSLIST, co};

    let own_pid = std::process::id();
    let Ok(mut snapshot) = HPROCESSLIST::CreateToolhelp32Snapshot(co::TH32CS::SNAPPROCESS, None)
    else {
        return false;
    };
    let parent_pid = snapshot
        .iter_processes()
        .filter_map(Result::ok)
        .find(|entry| entry.th32ProcessID == own_pid)
        .map(|entry| entry.th32ParentProcessID);
    let Some(parent_pid) = parent_pid else {
        return false;
    };
    let Ok(parent) = HPROCESS::OpenProcess(
        co::PROCESS::SYNCHRONIZE | co::PROCESS::QUERY_LIMITED_INFORMATION,
        false,
        parent_pid,
    ) else {
        // Not accessible (or already gone): keep serving until stdin reaches end of file.
        return false;
    };
    let parent_created = parent.GetProcessTimes().ok().map(|times| filetime(times.0));
    let own_created = HPROCESS::GetCurrentProcess()
        .GetProcessTimes()
        .ok()
        .map(|times| filetime(times.0));
    if let (Some(parent_created), Some(own_created)) = (parent_created, own_created)
        && parent_created > own_created
    {
        // The pid was reused by a newer process: the real parent is already gone.
        return true;
    }
    parent.WaitForSingleObject(None).is_ok()
}

#[cfg(windows)]
fn filetime(time: winsafe::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(not(windows))]
async fn parent_exit() {
    // The server only serves on native Windows (spec §3); elsewhere stdin end of file suffices.
    std::future::pending::<()>().await
}

