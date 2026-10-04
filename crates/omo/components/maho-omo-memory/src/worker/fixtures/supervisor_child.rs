use std::{collections::BTreeMap, path::Path};
use super::super::run_artifacts::{read_run_json, write_run_json_atomic};
pub async fn run(mode: &str, run_dir: &Path, env: &BTreeMap<String, String>) -> Result<i32, String> {
    if mode == "inspect" {
        let ledger: serde_json::Value = read_run_json(&run_dir.join("ledger.json")).map_err(|error| error.to_string())?;
        write_run_json_atomic(&run_dir.join("child-observation.json"), &serde_json::json!({"pid":ledger["pid"],"processStart":ledger["processStart"],"childPid":ledger["childPid"],"childProcessStart":ledger["childProcessStart"]}), 0o600).map_err(|error| error.to_string())?;
        return Ok(23);
    }
    if mode == "model-not-found" {
        eprintln!("Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.");
        return Ok(1);
    }
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|error| error.to_string())?;
    let _exit_socket = match env.get("OMO_MEMORY_SUPERVISOR_EXIT_PORT").and_then(|port| port.parse::<u16>().ok()) {
        Some(port) => Some(tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.map_err(|error| error.to_string())?),
        None => None,
    };
    write_run_json_atomic(&run_dir.join("child-started.json"), &serde_json::json!({"pid":std::process::id()}), 0o600).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    while term.recv().await.is_some() {
        if mode == "graceful" || mode == "stubborn" {
            write_run_json_atomic(&run_dir.join("child-terminated.json"), &serde_json::json!({"signal":"SIGTERM"}), 0o600).map_err(|error| error.to_string())?;
            if mode == "graceful" { return Ok(0); }
        }
    }
    std::future::pending().await
}
