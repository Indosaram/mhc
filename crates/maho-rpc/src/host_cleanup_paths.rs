//! What a host child removes when its supervisor dies without cleaning up, in removal order
//! (senpi `rpc/host-cleanup-paths.ts`). The registration pointer comes LAST: every reader takes
//! "no pointer" to mean "this endpoint has no host", so the pointer may only disappear once
//! everything it describes is already gone (senpi#2241).
use std::path::PathBuf;

pub struct HostCleanupTargets {
    pub pointer_file: PathBuf,
    pub generation_pid_file: PathBuf,
    pub settings_file: PathBuf,
    pub public_socket: PathBuf,
    /// A successor writes no registration of its own until the ensure that spawned it does, and the
    /// files under these paths still describe the generation being replaced.
    pub successor: bool,
    /// Node platform name (`win32`, `linux`, `darwin`).
    pub platform: &'static str,
}

pub fn host_crash_cleanup_paths(targets: &HostCleanupTargets) -> Vec<PathBuf> {
    // POSIX public sockets are removed ownership-checked by the host child, never by path from a
    // crash-path cleanup: a blind removal here would unlink a newer host's freshly published entry
    // after a takeover. Windows named pipes have no filesystem entry to own, so they stay listed.
    let public_entry: Vec<PathBuf> = if targets.platform == "win32" {
        vec![targets.public_socket.clone()]
    } else {
        Vec::new()
    };
    if targets.successor {
        return public_entry;
    }
    let mut paths = vec![
        targets.generation_pid_file.clone(),
        targets.settings_file.clone(),
    ];
    paths.extend(public_entry);
    paths.push(targets.pointer_file.clone());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(successor: bool, platform: &'static str) -> HostCleanupTargets {
        HostCleanupTargets {
            pointer_file: "pointer".into(),
            generation_pid_file: "pid".into(),
            settings_file: "settings".into(),
            public_socket: "public".into(),
            successor,
            platform,
        }
    }

    #[test]
    fn pointer_is_removed_last_and_posix_socket_never() {
        let paths = host_crash_cleanup_paths(&targets(false, "linux"));
        assert_eq!(paths, ["pid", "settings", "pointer"].map(PathBuf::from));
        let paths = host_crash_cleanup_paths(&targets(false, "win32"));
        assert_eq!(
            paths,
            ["pid", "settings", "public", "pointer"].map(PathBuf::from)
        );
    }

    #[test]
    fn a_successor_leaves_the_replaced_generation_files() {
        assert!(host_crash_cleanup_paths(&targets(true, "linux")).is_empty());
        assert_eq!(
            host_crash_cleanup_paths(&targets(true, "win32")),
            vec![PathBuf::from("public")]
        );
    }
}
