//! Port of omo `packages/senpi-task/src/runners/rpc-host/own-endpoint.ts`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::runners::rpc_host::session_role::read_session_context;

pub const HOST_SOCKET_CONTEXT: &str = "host_socket";

pub fn read_own_host_socket(pi: &Value) -> Option<String> {
    read_session_context(pi)?
        .get(HOST_SOCKET_CONTEXT)
        .filter(|socket| !socket.is_empty())
        .cloned()
}

pub fn is_own_endpoint(socket: &str, own_host_socket: Option<&str>) -> bool {
    let Some(own) = own_host_socket else {
        return false;
    };
    canonical_socket_path(socket) == canonical_socket_path(own)
}

pub fn attach_only_endpoint(
    socket: &str,
    own_host_socket: Option<&str>,
    inside_host: bool,
) -> bool {
    match own_host_socket {
        None => inside_host,
        Some(own) => is_own_endpoint(socket, Some(own)),
    }
}

/// The rule the host stamps `host_socket` with: the directory canonicalised through its deepest
/// EXISTING ancestor, the missing tail re-appended verbatim, then the socket's basename - so a shard
/// whose `rpc/shards/` does not exist yet still compares equal across `/tmp` spellings.
fn canonical_socket_path(socket: &str) -> PathBuf {
    let resolved = absolute(socket);
    let directory = resolved.parent().unwrap_or(&resolved).to_path_buf();
    let name = resolved
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .map(PathBuf::from)
        .unwrap_or_default();
    canonical_directory(&directory).join(name)
}

fn absolute(path: &str) -> PathBuf {
    if Path::new(path).is_absolute() {
        return normalize_lexical(Path::new(path));
    }
    std::path::absolute(path)
        .map(|absolute| normalize_lexical(&absolute))
        .unwrap_or_else(|_| normalize_lexical(Path::new(path)))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.parent().is_some() {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn canonical_directory(directory: &Path) -> PathBuf {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut existing = directory.to_path_buf();
    loop {
        if existing.exists() {
            break;
        }
        let Some(parent) = existing.parent().map(Path::to_path_buf) else {
            return directory.to_path_buf();
        };
        if parent.as_os_str().is_empty() {
            return std::fs::canonicalize(&existing).unwrap_or_else(|_| directory.to_path_buf());
        }
        missing.push(
            existing
                .file_name()
                .map(std::ffi::OsStr::to_os_string)
                .unwrap_or_default(),
        );
        existing = parent;
    }
    let Ok(mut canonical) = std::fs::canonicalize(&existing) else {
        return directory.to_path_buf();
    };
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    canonical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_without_context_has_no_own_socket() {
        assert_eq!(read_own_host_socket(&Value::Null), None);
        assert_eq!(
            read_own_host_socket(&serde_json::json!({"sessionContext": {}})),
            None
        );
        assert_eq!(
            read_own_host_socket(&serde_json::json!({"sessionContext": {"host_socket": ""}})),
            None
        );
    }

    #[test]
    fn the_stamped_socket_is_read_back() {
        assert_eq!(
            read_own_host_socket(
                &serde_json::json!({"sessionContext": {"host_socket": "/rpc/shards/i-abc.sock"}})
            )
            .as_deref(),
            Some("/rpc/shards/i-abc.sock")
        );
    }

    #[cfg(unix)]
    fn spellings() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = std::fs::canonicalize(dir.path())
            .expect("canonical root")
            .join("real");
        std::fs::create_dir_all(&real).expect("real dir");
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("symlinked ancestor");
        (dir, real, alias)
    }

    #[cfg(unix)]
    #[test]
    fn the_own_socket_and_a_record_naming_it_through_a_symlinked_directory_agree() {
        let (_dir, real, alias) = spellings();
        assert!(is_own_endpoint(
            &alias.join("p-a.sock").to_string_lossy(),
            Some(&real.join("p-a.sock").to_string_lossy())
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_shard_directory_that_does_not_exist_yet_still_compares_through_a_symlinked_ancestor() {
        let (_dir, real, alias) = spellings();
        assert!(is_own_endpoint(
            &alias
                .join("rpc")
                .join("shards")
                .join("p-a.sock")
                .to_string_lossy(),
            Some(
                &real
                    .join("rpc")
                    .join("shards")
                    .join("p-a.sock")
                    .to_string_lossy()
            )
        ));
    }

    #[test]
    fn a_missing_directory_spelled_with_a_dot_dot_segment_still_agrees() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = std::fs::canonicalize(dir.path()).expect("canonical root");
        let spelled = format!("{}/gone/../shards/p-a.sock", real.to_string_lossy());
        assert!(is_own_endpoint(
            &spelled,
            Some(&real.join("shards").join("p-a.sock").to_string_lossy())
        ));
    }

    #[test]
    fn another_endpoint_is_not_this_session_endpoint() {
        assert!(!is_own_endpoint("/rpc/shards/p-1.sock", None));
        assert!(!is_own_endpoint(
            "/rpc/shards/p-1.sock",
            Some("/rpc/shards/p-2.sock")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_different_socket_in_the_same_directory_is_not_the_own_endpoint() {
        let (_dir, real, alias) = spellings();
        assert!(!is_own_endpoint(
            &alias.join("shards").join("p-b.sock").to_string_lossy(),
            Some(&real.join("shards").join("p-a.sock").to_string_lossy())
        ));
    }

    #[test]
    fn an_unknown_own_socket_falls_back_to_the_inside_host_fact() {
        assert!(attach_only_endpoint("/rpc/shards/p-1.sock", None, true));
        assert!(!attach_only_endpoint("/rpc/shards/p-1.sock", None, false));
        assert!(attach_only_endpoint(
            "/rpc/shards/p-1.sock",
            Some("/rpc/shards/p-1.sock"),
            false
        ));
    }
}
