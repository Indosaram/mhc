//! The naming contract of a terminal's control socket, `t-<sha256(instanceId)[:16]>.sock`
//! (senpi `rpc/tui-socket.ts`). A TUI endpoint authenticates every connection with the 32-byte secret
//! beside it on every platform, so a client sends that secret first; hosts keep their own rule (a
//! handshake on win32 only), which is why the decision reads the name.
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn tui_socket_name(instance_id: &str) -> String {
    let digest = hex::encode(Sha256::digest(instance_id.as_bytes()));
    format!("t-{}.sock", &digest[..16])
}

pub fn is_tui_control_socket(socket: &str) -> bool {
    let name = Path::new(socket)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(socket);
    let Some(hash) = name
        .strip_prefix("t-")
        .and_then(|rest| rest.strip_suffix(".sock"))
    else {
        return false;
    };
    hash.len() == 16
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `platform` uses Node's names (`win32`, `linux`, `darwin`).
pub fn socket_needs_handshake(socket: &str, platform: &str) -> bool {
    platform == "win32" || is_tui_control_socket(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_the_sha256_prefix_of_the_instance_id() {
        let name = tui_socket_name("instance-1");
        let digest = hex::encode(Sha256::digest(b"instance-1"));
        assert_eq!(name, format!("t-{}.sock", &digest[..16]));
        assert!(is_tui_control_socket(&format!("/tmp/run/{name}")));
    }

    #[test]
    fn host_sockets_need_a_handshake_only_on_win32() {
        assert!(!socket_needs_handshake("/tmp/run/host.sock", "linux"));
        assert!(socket_needs_handshake("/tmp/run/host.sock", "win32"));
        assert!(socket_needs_handshake(
            "/tmp/run/t-0123456789abcdef.sock",
            "linux"
        ));
        assert!(!is_tui_control_socket("t-0123456789ABCDEF.sock"));
        assert!(!is_tui_control_socket("t-0123456789abcde.sock"));
    }
}
