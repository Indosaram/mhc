//! Session-holder identity (senpi `core/session-hold-errors.ts`).
use serde::{Deserialize, Serialize};

/// Public RPC identity, supplied by the host rather than the client's opaque context.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionHolderEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_socket: Option<String>,
    pub host_instance: String,
    /// The daemon directory of that generation; kept in the holder record only, never reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_daemon_dir: Option<String>,
}
impl SessionHolderEndpoint {
    /// This host's endpoint, carrying `SENPI_RPC_HOST_DAEMON_DIR` when set so an unprovable lease is reclaimed once its generation ended.
    pub fn from_env(host_socket: Option<String>, host_instance: String) -> Self {
        let host_daemon_dir = std::env::var(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV)
            .ok()
            .filter(|dir| !dir.is_empty());
        Self {
            host_socket,
            host_instance,
            host_daemon_dir,
        }
    }
    /// The endpoint as reported to clients: never the daemon directory.
    pub fn reported(&self) -> serde_json::Value {
        let mut value = serde_json::json!({"host_instance":self.host_instance});
        if let Some(socket) = &self.host_socket {
            value["host_socket"] = socket.clone().into();
        }
        value
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daemon_dir_is_kept_but_never_reported() {
        let endpoint = SessionHolderEndpoint {
            host_socket: Some("/s".into()),
            host_instance: "i".into(),
            host_daemon_dir: Some("/d".into()),
        };
        assert_eq!(
            endpoint.reported(),
            serde_json::json!({"host_instance":"i","host_socket":"/s"})
        );
        assert_eq!(
            serde_json::to_value(&endpoint).unwrap()["host_daemon_dir"],
            "/d"
        );
    }
}
