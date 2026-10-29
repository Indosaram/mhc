//! `mhc-ast-grep`: structural search/rewrite/scan tools served over stdio MCP
//! (`docs/cleanroom/specs/ast-grep.md`). Started as `mhc-ast-grep mcp`; any other first argument
//! prints usage and exits 2.

use maho_ast_grep::server::ServerOptions;
use maho_ast_grep::{Server, run_cli};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let server = Server::new(ServerOptions::from_env());
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let code = run_cli(&args, &server, stdin, stdout, &mut stderr, parent_exit()).await;
    // Exit directly: the stdin reader and the parent watch live on blocking threads.
    std::process::exit(code)
}

/// Resolves when the parent process has exited.
#[cfg(unix)]
async fn parent_exit() {
    let parent = std::os::unix::process::parent_id();
    if parent <= 1 {
        std::future::pending::<()>().await;
    }
    // Unix gives no wait handle for a parent: a changed parent id means it exited and the
    // process was reparented, so watch the id on a coarse timer.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if std::os::unix::process::parent_id() != parent {
            return;
        }
    }
}

#[cfg(windows)]
async fn parent_exit() {
    match tokio::task::spawn_blocking(wait_for_parent).await {
        Ok(true) => {}
        Ok(false) | Err(_) => std::future::pending::<()>().await,
    }
}

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
        return false;
    };
    parent.WaitForSingleObject(None).is_ok()
}
