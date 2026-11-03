//! The supervisor's private hop (senpi `host-lifecycle-internal-key.ts`, senpi#3795): its path, the transport
//! credentials the child, the activity observer and the public proxy share (a Windows pipe secret, or a fresh
//! memory-only POSIX key the child gets on stdin), and the supervisor's own start for the child's lifetime poll.
use crate::host_lifecycle::{InternalSocketPath, create_internal_socket_path};

/// The supervisor's start on the child's clock: `/proc` start ticks on Linux, the recorded start time elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupervisorStart {
    pub time: Option<String>,
    pub ticks: Option<String>,
}

pub struct InternalHop {
    pub internal: InternalSocketPath,
    pub internal_secret: Option<Vec<u8>>,
    pub internal_key: Option<[u8; 32]>,
    pub lifetime_binding: Option<crate::host_lifetime_binding::PreparedLifetimeBinding>,
    pub supervisor_start: Option<SupervisorStart>,
}

/// The start time the hop directory's owner record already holds, so it is read once, not twice.
fn recorded_owner_start_time(dir: Option<&std::path::Path>) -> Option<String> {
    let text = std::fs::read_to_string(dir?.join(".owner")).ok()?;
    let owner: serde_json::Value = serde_json::from_str(&text).ok()?;
    // An unreadable record leaves the child's identity poll without a start time: unknown, never death.
    owner
        .get("processStartTime")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// senpi `prepareInternalHop`.
pub fn prepare_internal_hop(
    daemon_dir: &std::path::Path,
    platform: &str,
) -> std::io::Result<InternalHop> {
    let internal = create_internal_socket_path(daemon_dir, platform)?;
    let internal_secret = if platform == "win32" {
        let secret_path = internal
            .secret_path
            .as_deref()
            .ok_or_else(|| std::io::Error::other("internal socket has no secret path"))?;
        Some(crate::socket_transport::create_socket_secret(secret_path)?.to_vec())
    } else {
        None
    };
    let internal_key = if platform == "win32" {
        None
    } else {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|error| std::io::Error::other(error.to_string()))?;
        Some(key)
    };
    let lifetime_binding = internal_key
        .and(internal.dir.as_deref())
        .and_then(|dir| crate::host_lifetime_binding::prepare_lifetime_binding(dir, platform));
    let supervisor_start = internal_key.map(|_| SupervisorStart {
        time: recorded_owner_start_time(internal.dir.as_deref()),
        ticks: crate::process_start_identity::read_process_start_ticks(std::process::id()),
    });
    Ok(InternalHop {
        internal,
        internal_secret,
        internal_key,
        lifetime_binding,
        supervisor_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_owner_start_comes_from_the_hop_record_and_a_bad_record_is_unknown() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(recorded_owner_start_time(Some(dir.path())), None);
        std::fs::write(dir.path().join(".owner"), r#"{"processStartTime":"Mon 1"}"#).expect("w");
        assert_eq!(
            recorded_owner_start_time(Some(dir.path())).as_deref(),
            Some("Mon 1")
        );
        std::fs::write(dir.path().join(".owner"), "not json").expect("w");
        assert_eq!(recorded_owner_start_time(Some(dir.path())), None);
    }
}
