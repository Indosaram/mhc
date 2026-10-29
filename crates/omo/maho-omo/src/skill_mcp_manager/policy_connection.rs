use super::types::McpClient;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
pub const MCP_ENVIRONMENT_POLICY_CHANGED: &str =
    "MCP environment policy changed during connection; retry with the current grant snapshot.";
pub async fn await_policy_signal<T>(
    future: impl std::future::Future<Output = super::Result<T>>,
    mut signal: Option<watch::Receiver<bool>>,
) -> super::Result<T> {
    tokio::select! {biased;
        ()=async {match signal.as_mut(){Some(signal)=>{if !*signal.borrow(){let _=signal.changed().await;}},None=>std::future::pending::<()>().await}}=>Err(MCP_ENVIRONMENT_POLICY_CHANGED.into()),
        result=future=>result,
    }
}
pub async fn connect_with_policy_signal(
    client: &Arc<McpClient>,
    mut signal: Option<watch::Receiver<bool>>,
) -> super::Result<()> {
    let cancelled = async {
        match signal.as_mut() {
            Some(signal) => {
                if !*signal.borrow() {
                    let _ = signal.changed().await;
                }
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {biased;
        ()=cancelled=>{if let Err(error)=client.close().await {eprintln!("MCP policy close: {error}");}Err(MCP_ENVIRONMENT_POLICY_CHANGED.into())},
        result=client.initialize(Duration::from_secs(30))=>result.map_err(|e|e.to_string()),
    }
}
