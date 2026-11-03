//! How an endpoint is NAMED (senpi `rpc/host-endpoint-names.ts`): its canonical spelling, the daemon
//! directory name every client derives from it, and the shard socket names. The shard key and parser
//! live in `host_endpoints` and are re-exported here.
use crate::session_path_key::canonical_session_path;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The one spelling of an endpoint every client derives the same way. On POSIX: the socket path with
/// its directory resolved through its deepest existing ancestor; a path already spelled that way comes
/// back unchanged. On win32: the normalized lower-cased path. An abstract socket has no directory to
/// resolve. Deliberately total.
pub fn canonical_endpoint_path(socket: &str, platform: &str) -> String {
    if platform == "win32" {
        return win32_normalize(socket).to_lowercase();
    }
    if socket.starts_with('\0') {
        return socket.to_owned();
    }
    let path = Path::new(socket);
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let base = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    canonical_session_path(&parent)
        .join(base)
        .to_string_lossy()
        .into_owned()
}

/// `path.win32.normalize`: forward slashes become backslashes, repeated separators collapse and `.`/`..`
/// segments resolve lexically; a trailing separator is kept.
fn win32_normalize(path: &str) -> String {
    let unified = path.replace('/', "\\");
    let device = ["\\\\.\\", "\\\\?\\"]
        .into_iter()
        .find(|device| unified.starts_with(device));
    let (prefix, rest) = if let Some(device) = device {
        (device.to_owned(), unified[device.len()..].to_owned())
    } else if let Some(rest) = unified.strip_prefix("\\\\") {
        ("\\\\".to_owned(), rest.to_owned())
    } else if unified.len() >= 2 && unified.as_bytes()[1] == b':' {
        let (drive, rest) = unified.split_at(2);
        let rooted = rest.starts_with('\\');
        (
            format!("{drive}{}", if rooted { "\\" } else { "" }),
            rest.trim_start_matches('\\').to_owned(),
        )
    } else if let Some(rest) = unified.strip_prefix('\\') {
        ("\\".to_owned(), rest.to_owned())
    } else {
        (String::new(), unified.clone())
    };
    let rooted = !prefix.is_empty();
    let mut parts: Vec<&str> = Vec::new();
    for segment in rest.split('\\') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = format!("{prefix}{}", parts.join("\\"));
    if out.is_empty() {
        out.push('.');
    } else if unified.ends_with('\\') && !out.ends_with('\\') {
        out.push('\\');
    }
    out
}

fn directory_name_of(endpoint: &str) -> String {
    hex::encode(Sha256::digest(endpoint.as_bytes()))[..16].to_owned()
}

/// `sha256(<canonical endpoint>)[:16]`: every spelling of one endpoint shares one directory.
pub fn daemon_directory_name(socket: &str, platform: &str) -> String {
    directory_name_of(&canonical_endpoint_path(socket, platform))
}

/// Whether two spellings name one endpoint.
pub fn same_endpoint(socket: &str, other: &str, platform: &str) -> bool {
    socket == other
        || canonical_endpoint_path(socket, platform) == canonical_endpoint_path(other, platform)
}

/// Whether a record naming `socket` belongs in the directory called `name`: the canonical name, or the
/// name a build that hashed the spelling itself gave it (that directory must stay collectable).
pub fn socket_names_directory(socket: &str, name: &str, platform: &str) -> bool {
    daemon_directory_name(socket, platform) == name
        || (platform != "win32" && directory_name_of(socket) == name)
}

pub use crate::host_endpoints::{
    ShardKind, parse_shard_socket, shard_key, shard_socket_path_for_key,
};

pub fn shard_socket_path(root: &Path, kind: ShardKind, owner_id: &str) -> PathBuf {
    shard_socket_path_for_key(root, kind, &shard_key(kind, owner_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_endpoint_shares_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = maho_core::paths::strip_verbatim_prefix(
            std::fs::canonicalize(dir.path()).expect("canonical"),
        );
        let canonical = real.join("host.sock").to_string_lossy().into_owned();
        let dotted = format!("{}/./host.sock", dir.path().display());
        assert_eq!(canonical_endpoint_path(&dotted, "linux"), canonical);
        assert_eq!(canonical_endpoint_path(&canonical, "linux"), canonical);
        assert!(same_endpoint(&dotted, &canonical, "linux"));
        assert_eq!(
            daemon_directory_name(&dotted, "linux"),
            directory_name_of(&canonical)
        );
        assert!(socket_names_directory(
            &dotted,
            &directory_name_of(&dotted),
            "linux"
        ));
    }

    #[test]
    fn abstract_and_win32_spellings() {
        assert_eq!(canonical_endpoint_path("\0abstract", "linux"), "\0abstract");
        assert_eq!(
            canonical_endpoint_path("\\\\.\\PIPE\\Senpi//X", "win32"),
            "\\\\.\\pipe\\senpi\\x"
        );
        assert_eq!(win32_normalize("C:/a/./b/../c"), "C:\\a\\c");
    }

    #[test]
    fn shard_names_round_trip() {
        let path = shard_socket_path(Path::new("/run"), ShardKind::I, "thread-1");
        let key = shard_key(ShardKind::I, "thread-1");
        assert_eq!(path, PathBuf::from(format!("/run/i-{key}.sock")));
        assert_eq!(
            parse_shard_socket(&path.to_string_lossy()),
            Some((ShardKind::I, key))
        );
        assert_eq!(parse_shard_socket("/run/x-0123456789abcdef.sock"), None);
        assert_eq!(parse_shard_socket("/run/p-0123456789ABCDEF.sock"), None);
    }
}
