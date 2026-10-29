//! Local TCP port availability probing.

use std::fmt;
use std::net::TcpListener;

pub const DEFAULT_SERVER_PORT: u16 = 4096;
pub const DEFAULT_PORT_HOSTNAME: &str = "127.0.0.1";
const MAX_PORT_ATTEMPTS: u16 = 20;

/// True when a listener can bind `hostname:port` right now (the probe socket is released).
pub fn is_port_available(port: u16, hostname: &str) -> bool {
    TcpListener::bind((hostname, port)).is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoAvailablePortError {
    pub start_port: u16,
    pub end_port: u16,
}

impl fmt::Display for NoAvailablePortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "No available port found in range {}-{}",
            self.start_port, self.end_port
        )
    }
}

impl std::error::Error for NoAvailablePortError {}

pub fn find_available_port(start_port: u16, hostname: &str) -> Result<u16, NoAvailablePortError> {
    (0..MAX_PORT_ATTEMPTS)
        .filter_map(|attempt| start_port.checked_add(attempt))
        .find(|port| is_port_available(*port, hostname))
        .ok_or(NoAvailablePortError {
            start_port,
            end_port: start_port.saturating_add(MAX_PORT_ATTEMPTS - 1),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoPortResult {
    pub port: u16,
    pub was_auto_selected: bool,
}

pub fn get_available_server_port(
    preferred_port: u16,
    hostname: &str,
) -> Result<AutoPortResult, NoAvailablePortError> {
    if is_port_available(preferred_port, hostname) {
        return Ok(AutoPortResult {
            port: preferred_port,
            was_auto_selected: false,
        });
    }
    let port = find_available_port(preferred_port.saturating_add(1), hostname)?;
    Ok(AutoPortResult {
        port,
        was_auto_selected: true,
    })
}
