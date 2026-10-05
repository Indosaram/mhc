use super::daemon_probe::probe_listen;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum AppServerListenOccupancy {
    AppServer { version: String },
    Available,
}
pub async fn inspect_listen_occupancy(token_file: &Path, listen: &Value, version: &str) -> Result<AppServerListenOccupancy, std::io::Error> {
    if let Some(version) = probe_listen(token_file, listen, 2000, version).await? {
        return Ok(AppServerListenOccupancy::AppServer { version });
    }
    if listen["kind"] == "ws" {
        let host = listen["host"].as_str().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput,"Missing host"))?;
        let port = listen["port"].as_u64().and_then(|port| u16::try_from(port).ok()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput,"Invalid port"))?;
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(listener) => drop(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => return Err(std::io::Error::new(error.kind(), format!("EADDRINUSE: app-server daemon cannot listen on {}; the TCP address is occupied by a listener that did not answer initialize.",listen["url"].as_str().unwrap_or_default()))),
            Err(error) => return Err(error),
        }
    }
    Ok(AppServerListenOccupancy::Available)
}
