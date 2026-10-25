use super::{environment_grant_policy::EnvironmentGrantPolicy, types::SharedState};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
async fn close(client: Arc<super::types::McpClient>) {
    if let Err(error) = client.close().await {
        eprintln!("MCP cleanup: {error}");
    }
}
pub async fn disconnect_session(state: &SharedState, session_id: &str) {
    let prefix = format!("{session_id}:");
    let clients = {
        let mut state = state.lock().await;
        *state
            .disconnected_sessions
            .entry(session_id.into())
            .or_default() += 1;
        state
            .pending_connections
            .retain(|key, _| !key.starts_with(&prefix));
        let keys: Vec<_> = state
            .clients
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| state.clients.remove(&k))
            .collect::<Vec<_>>()
    };
    for managed in clients {
        close(managed.client).await;
    }
}
pub async fn disconnect_all(state: &SharedState, policy: &EnvironmentGrantPolicy) {
    policy.close().await;
    let clients = {
        let mut state = state.lock().await;
        state.shutdown_generation += 1;
        state.disposed = true;
        state.pending_connections.clear();
        state.disconnected_sessions.clear();
        std::mem::take(&mut state.clients)
    };
    for managed in clients.into_values() {
        close(managed.client).await;
    }
    state.lock().await.disposed = false;
}
pub async fn force_reconnect(state: &SharedState, client_key: &str) -> bool {
    let clients = {
        let mut state = state.lock().await;
        let keys: Vec<_> = state
            .clients
            .keys()
            .filter(|key| {
                key.as_str() == client_key || key.starts_with(&format!("{client_key}::identity="))
            })
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| state.clients.remove(&k))
            .collect::<Vec<_>>()
    };
    let closed = !clients.is_empty();
    for managed in clients {
        close(managed.client).await;
    }
    closed
}
pub fn start_cleanup_timer(state: &SharedState) -> tokio::task::JoinHandle<()> {
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        loop {
            timer.tick().await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let clients = {
                let mut state = state.lock().await;
                let keys: Vec<_> = state
                    .clients
                    .iter()
                    .filter(|(_, c)| {
                        Instant::now().duration_since(c.last_used_at) > Duration::from_secs(300)
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                keys.into_iter()
                    .filter_map(|k| state.clients.remove(&k))
                    .collect::<Vec<_>>()
            };
            for managed in clients {
                close(managed.client).await;
            }
        }
    })
}

pub fn register_process_cleanup(
    state: &SharedState,
    policy: &Arc<EnvironmentGrantPolicy>,
) -> tokio::task::JoinHandle<()> {
    let state = Arc::downgrade(state);
    let policy = Arc::downgrade(policy);
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut interrupt =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        eprintln!("MCP interrupt registration: {error}");
                        return;
                    }
                };
            let mut terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        eprintln!("MCP termination registration: {error}");
                        return;
                    }
                };
            tokio::select! {_=interrupt.recv()=>{},_=terminate.recv()=>{}}
        }
        #[cfg(not(unix))]
        {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("MCP interrupt registration: {error}");
                return;
            }
        }
        if let (Some(state), Some(policy)) = (state.upgrade(), policy.upgrade()) {
            disconnect_all(&state, &policy).await;
        }
    })
}
