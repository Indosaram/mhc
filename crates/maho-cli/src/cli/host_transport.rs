//! Assembly-owned half of senpi `packages/senpi-task/src/lifecycle/host-session-default.ts`: the
//! CLI's production `HostTransport` over the shared rpc-host daemon.
//!
//! `senpi_task::lifecycle::HostTransport` is the ONE surface the task lifecycle needs from the
//! rpc-host engine. Upstream reaches `runners/rpc-host/liveness` and `runners/rpc-host/close`
//! through `import()`; natively those are the `maho_rpc` host client, which is async, while the
//! trait is sync. This module is that bridge: dial the daemon socket, ask the two liveness questions
//! the lifecycle needs (`get_protocol_info`, `list_sessions { include_workers: true }`), and end a
//! session this process holds no handle for (`open` -> `abort` -> `close_session`).
//!
//! Behaviour of record: `packages/senpi-task/src/runners/rpc-host/{liveness,close}.ts` @ `455dee62`.

use std::path::Path;
use std::sync::Arc;

use senpi_task::lifecycle::{HostSessionCloseError, HostSessionCloseRequest, HostTransport};

/// `liveness.ts` `LIST_TIMEOUT_MS`.
const LIST_TIMEOUT_MS: u64 = 10_000;
/// `liveness.ts` `BUSY_CONNECT_TIMEOUT_MS`: bounded so a wedged accept cannot hold a draining host.
const BUSY_CONNECT_TIMEOUT_MS: u64 = 500;

/// Build the production host transport over a captured runtime handle.
///
/// The lifecycle's trait methods are sync and may be called from any thread (a tokio worker, a plain
/// OS thread, the main thread). `Handle::block_on` panics inside a runtime worker and
/// `block_in_place` panics on a current-thread runtime, so neither is usable here: every call runs
/// its future on a fresh OS thread that enters `handle` and blocks there, which is safe from any
/// caller context.
pub fn create_cli_host_transport(handle: tokio::runtime::Handle) -> Arc<dyn HostTransport> {
    create_cli_host_transport_with_agent_dir(handle, None)
}
pub fn create_cli_host_transport_with_agent_dir(
    handle: tokio::runtime::Handle,
    agent_dir: Option<std::path::PathBuf>,
) -> Arc<dyn HostTransport> {
    Arc::new(CliHostTransport { handle, agent_dir })
}

struct CliHostTransport {
    agent_dir: Option<std::path::PathBuf>,
    handle: tokio::runtime::Handle,
}

impl CliHostTransport {
    fn block_on<F>(&self, future: F) -> Result<F::Output, String>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = self.handle.clone();
        std::thread::spawn(move || handle.block_on(future))
            .join()
            .map_err(|_| "host transport call thread panicked".to_owned())
    }
}

async fn authenticated_client(
    socket: &str,
    agent_dir: Option<&Path>,
) -> Result<maho_rpc::rpc_client::RpcClient, String> {
    #[cfg(unix)]
    {
        let (socket, _) = maho_rpc::host_probe::authenticated_host_socket(
            socket,
            &serde_json::json!({"type":"get_protocol_info","observe":true}),
            LIST_TIMEOUT_MS,
            agent_dir,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(maho_rpc::rpc_client::RpcClient::from_stream(
            socket.into_stream().map_err(|error| error.to_string())?,
            Default::default(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = agent_dir;
        maho_rpc::rpc_client::RpcClient::connect(Path::new(socket))
            .await
            .map_err(|error| error.to_string())
    }
}
impl HostTransport for CliHostTransport {
    fn publish_task_status(
        &self,
        identity: &senpi_task::state::HostSessionIdentity,
        status: senpi_task::state::TaskStatus,
    ) -> Result<(), String> {
        let socket = identity.socket.clone();
        let path = identity.session_path.clone();
        let agent_dir = self.agent_dir.clone();
        self.block_on(async move {
            let client = authenticated_client(&socket,agent_dir.as_deref()).await?;
            let result = async {
                client.open_session(serde_json::json!({"sessionPath":path,"kind":"worker","context":{},"retainOnDisconnect":true,"autoTitle":false})).await.map_err(|error| error.to_string())?;
                client.set_session_context(std::collections::BTreeMap::from([("task_status".into(),Some(status.as_str().into()))])).await.map_err(|error| error.to_string())?;
                Ok(())
            }.await;
            client.stop().await;
            result
        })?
    }
    fn durable_session_id(
        &self,
        identity: &senpi_task::state::HostSessionIdentity,
    ) -> Result<Option<String>, String> {
        let socket = identity.socket.clone();
        let path = identity.session_path.clone();
        let agent_dir = self.agent_dir.clone();
        self.block_on(async move {
            #[cfg(unix)]
            let reply = maho_rpc::host_probe::ask_host_authenticated(
                &socket,
                &serde_json::json!({"type":"list_sessions","include_workers":true}),
                LIST_TIMEOUT_MS,
                agent_dir.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?
            .and_then(|reply| reply.get("data").cloned())
            .ok_or("host identity listing unanswered")?;
            #[cfg(not(unix))]
            let reply = {
                let _ = &agent_dir;
                maho_rpc::host_probe::request_on_socket(
                    &socket,
                    &serde_json::json!({"type":"list_sessions","include_workers":true}),
                    LIST_TIMEOUT_MS,
                )
                .await
                .ok_or("host identity listing unanswered")?
            };
            let sessions = reply["sessions"]
                .as_array()
                .ok_or("host identity listing malformed")?;
            Ok(sessions
                .iter()
                .find(|session| session["sessionPath"].as_str() == Some(path.as_str()))
                .and_then(|session| session["durableSessionId"].as_str())
                .map(str::to_owned))
        })?
    }

    fn daemon_reachable(&self, socket: &str) -> bool {
        let socket = socket.to_owned();
        self.block_on(async move { daemon_reachable(&socket).await })
            .unwrap_or(false)
    }

    fn live_session_paths_with_agent_dir(
        &self,
        socket: &str,
        agent_dir: Option<&Path>,
    ) -> Result<Vec<String>, String> {
        let socket = socket.to_owned();
        let agent_dir = agent_dir.map(ToOwned::to_owned);
        self.block_on(async move {
            live_session_paths_with_agent_dir(&socket, agent_dir.as_deref()).await
        })?
    }

    fn live_session_paths(&self, socket: &str) -> Result<Vec<String>, String> {
        self.live_session_paths_with_agent_dir(socket, self.agent_dir.as_deref())
    }

    fn close_host_session(
        &self,
        request: &HostSessionCloseRequest,
    ) -> Result<(), HostSessionCloseError> {
        let socket = request.host_session.socket.clone();
        let session_path = request.host_session.session_path.clone();
        let cwd = request.cwd.clone();
        let agent_dir = self.agent_dir.clone();
        match self.block_on(async move {
            close_host_session(&socket, &session_path, cwd.as_deref(), agent_dir.as_deref()).await
        }) {
            Ok(result) => result,
            Err(error) => Err(HostSessionCloseError { message: error }),
        }
    }
}

/// `liveness.daemonReachable`: the engine probe (`get_protocol_info`) answering means the daemon is
/// there; a socket that still accepts a connection while its loop is blocked is BUSY, not gone, so it
/// counts too (omo#9069). Both go through the ported `host_probe`, the same probe the ensure path
/// uses.
async fn daemon_reachable(socket: &str) -> bool {
    if maho_rpc::host_probe::probe_protocol_info(
        socket,
        maho_rpc::host_probe::DEFAULT_PROBE_TIMEOUT_MS,
    )
    .await
    .is_some()
    {
        return true;
    }
    maho_rpc::host_probe::probe_socket_reachable(socket, BUSY_CONNECT_TIMEOUT_MS).await
}

/// `liveness.liveSessionPaths`: asked on the wire, never through `RpcClient::list_sessions` (that
/// sends a bare `list_sessions` and drops `include_workers`, so every task child would read as gone
/// and be claimed by the next reconciling session - #8932). An unanswered or malformed listing is an
/// ERROR, never an empty list: "could not ask" must stay distinguishable from "holds nothing"
/// (omo#9450).
async fn live_session_paths_with_agent_dir(
    socket: &str,
    agent_dir: Option<&Path>,
) -> Result<Vec<String>, String> {
    #[cfg(unix)]
    let reply = maho_rpc::host_probe::ask_host_authenticated(
        socket,
        &serde_json::json!({ "type": "list_sessions", "include_workers": true }),
        LIST_TIMEOUT_MS,
        agent_dir,
    )
    .await
    .map_err(|error| error.to_string())?
    .and_then(|reply| reply.get("data").cloned());
    #[cfg(not(unix))]
    let reply = {
        let _ = agent_dir;
        maho_rpc::host_probe::request_on_socket(
            socket,
            &serde_json::json!({ "type": "list_sessions", "include_workers": true }),
            LIST_TIMEOUT_MS,
        )
        .await
    };
    let Some(reply) = reply else {
        return Err(format!("list_sessions failed on {socket}"));
    };
    let Some(sessions) = reply.get("sessions").and_then(serde_json::Value::as_array) else {
        return Err(format!(
            "list_sessions on {socket} answered without a session list"
        ));
    };
    Ok(sessions
        .iter()
        .filter_map(|row| {
            row.get("sessionPath")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

/// `close.closeHostSession`: attach to the recorded session path, abort whatever turn is in flight,
/// then `close_session`. `Ok` ONLY when the daemon confirmed the close; a refused attach or
/// close_session is `Err`. It NEVER signals a pid - the only pid on the other end is the daemon's.
async fn close_host_session(
    socket: &str,
    session_path: &str,
    cwd: Option<&str>,
    agent_dir: Option<&Path>,
) -> Result<(), HostSessionCloseError> {
    let cwd = cwd.map(str::to_owned).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let client = authenticated_client(socket, agent_dir)
        .await
        .map_err(|error| HostSessionCloseError {
            message: format!("could not attach: {error}"),
        })?;
    // Re-attaching to close: `retainOnDisconnect` false so a failure mid-teardown cannot leave the
    // session pinned, and the worker kind keeps it out of every interactive session list.
    client
        .open_session(serde_json::json!({
            "sessionPath": session_path,
            "cwd": cwd,
            "kind": "worker",
            "context": {},
            "retainOnDisconnect": false,
            "autoTitle": false,
        }))
        .await
        .map_err(|error| HostSessionCloseError {
            message: format!("could not attach: {error}"),
        })?;
    // The abort carries the just-opened session's id (the frame's `sessionId`); a rejection is the
    // caller's to log, not fatal.
    if let Err(error) = client.abort().await {
        eprintln!("host close abort refused: {error}");
    }
    let result = client
        .close_session(None)
        .await
        .map_err(|error| HostSessionCloseError {
            message: format!("close_session rejected: {error}"),
        });
    client.stop().await;
    result
}
