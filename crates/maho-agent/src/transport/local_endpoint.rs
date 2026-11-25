//! Port of senpi `packages/agent/src/transport/local-endpoint.ts`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEndpoint {
    UnixSocket { address: String, mode: u32 },
    NamedPipe { address: String },
}

impl LocalEndpoint {
    pub fn address(&self) -> &str {
        match self {
            Self::UnixSocket { address, .. } | Self::NamedPipe { address } => address,
        }
    }
}

const PIPE_PREFIX: &str = r"\\.\pipe\";
const PIPE_MAX_LENGTH: usize = 256;
const OWNER_ONLY: u32 = 0o600;

pub fn local_endpoint(
    platform: &str,
    name: &str,
    socket_dir: &str,
) -> Result<LocalEndpoint, String> {
    if name.is_empty() || name.contains(['\\', '/']) {
        return Err(format!(
            "local endpoint: a name must be non-empty with no path separator, got {:?}",
            name
        ));
    }
    if platform == "win32" {
        let address = format!("{PIPE_PREFIX}{name}");
        if address.len() > PIPE_MAX_LENGTH {
            return Err(format!(
                "local endpoint: a pipe name is at most {PIPE_MAX_LENGTH} characters"
            ));
        }
        return Ok(LocalEndpoint::NamedPipe { address });
    }
    Ok(LocalEndpoint::UnixSocket {
        address: format!("{}/{name}.sock", socket_dir.trim_end_matches('/')),
        mode: OWNER_ONLY,
    })
}

/// The identity of an endpoint: Windows pipe names compare case-insensitively, socket paths exactly.
pub fn endpoint_key(endpoint: &LocalEndpoint) -> String {
    match endpoint {
        LocalEndpoint::NamedPipe { address } => address.to_ascii_uppercase(),
        LocalEndpoint::UnixSocket { address, .. } => address.clone(),
    }
}
