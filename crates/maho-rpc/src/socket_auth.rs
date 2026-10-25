//! Strict POSIX key records and mutual host authentication (`socket-auth.ts`).
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::Path,
};
#[derive(Debug, thiserror::Error)]
#[error("{code}:{location}: {detail}")]
pub struct RpcIdentityError {
    pub code: &'static str,
    pub location: String,
    pub detail: String,
}
fn error(code: &'static str, location: &str, detail: impl Into<String>) -> RpcIdentityError {
    RpcIdentityError {
        code,
        location: location.into(),
        detail: detail.into(),
    }
}
pub fn auth_required_error(endpoint: &str) -> RpcIdentityError {
    error(
        "auth_required",
        endpoint,
        "the host requires authentication; set SENPI_RPC_KEY_ROOT to the host's agent dir",
    )
}
fn hex32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
#[cfg(unix)]
fn check_directory(path: &Path, exact: Option<u32>) -> Result<(), RpcIdentityError> {
    use std::os::unix::fs::MetadataExt;
    let fail = |detail: String| error("unsafe_key_record", &path.display().to_string(), detail);
    let metadata = fs::symlink_metadata(path).map_err(|e| fail(e.to_string()))?;
    let uid = rustix::process::getuid().as_raw();
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid {
        return Err(fail("not a real directory owned by this user".into()));
    }
    let mode = metadata.mode() & 0o7777;
    if exact.map_or(mode & 0o022 != 0, |exact| mode != exact) {
        return Err(fail(format!("unsafe mode {mode:o}")));
    }
    Ok(())
}
#[cfg(unix)]
pub fn read_key(root: &Path, launch: &str) -> Result<Option<Vec<u8>>, RpcIdentityError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = root.join("rpc/keys").join(launch);
    for (directory, mode) in [
        (root.to_path_buf(), None),
        (root.join("rpc"), None),
        (root.join("rpc/keys"), Some(0o700)),
    ] {
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => check_directory(&directory, mode)?,
        }
    }
    let fail = |detail: String| error("unsafe_key_record", &path.display().to_string(), detail);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(fail(e.to_string())),
    };
    let metadata = file.metadata().map_err(|e| fail(e.to_string()))?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(fail("not a 0600 regular file owned by this user".into()));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| fail(e.to_string()))?;
    let row: Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
    let key = row["key"]
        .as_str()
        .filter(|key| hex32(key))
        .ok_or_else(|| fail("invalid key".into()))?;
    if row["version"] != 1 || row["launch_id"] != launch {
        return Err(fail("invalid version or launch".into()));
    }
    Ok(Some(hex::decode(key).map_err(|e| fail(e.to_string()))?))
}
pub fn proof(key: &[u8], label: &str, launch: &str, first: &str, second: &str) -> Vec<u8> {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner = block.map(|byte| byte ^ 0x36);
    let outer = block.map(|byte| byte ^ 0x5c);
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(format!("{label}\n{launch}\n{first}\n{second}"));
    let digest = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(digest);
    hash.finalize().to_vec()
}
pub fn verify_server(
    key: &[u8],
    auth: &Value,
    nonce: &str,
    endpoint: &str,
) -> Result<(), RpcIdentityError> {
    let launch = auth["launch_id"]
        .as_str()
        .ok_or_else(|| error("host_identity_unproven", endpoint, "missing launch"))?;
    let server = auth["server_nonce"]
        .as_str()
        .filter(|v| hex32(v))
        .ok_or_else(|| error("host_identity_unproven", endpoint, "invalid nonce"))?;
    let expected = proof(key, "senpi-rpc-server", launch, nonce, server);
    let actual = auth["server_proof"]
        .as_str()
        .and_then(|v| hex::decode(v).ok())
        .unwrap_or_default();
    use subtle::ConstantTimeEq;
    if !bool::from(expected.as_slice().ct_eq(actual.as_slice())) {
        return Err(error(
            "host_identity_unproven",
            endpoint,
            "wrong server proof",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_proof_binds_both_nonces_and_launch() {
        let key = [7u8; 32];
        let launch = "38e99a77-09e9-49d8-80a0-3eae08288cb4";
        let nonce = "a".repeat(64);
        let server = "b".repeat(64);
        let auth = serde_json::json!({"launch_id":launch,"server_nonce":server,"server_proof":hex::encode(proof(&key,"senpi-rpc-server",launch,&nonce,&server))});
        assert!(verify_server(&key, &auth, &nonce, "endpoint").is_ok());
        assert!(verify_server(&key, &auth, &"c".repeat(64), "endpoint").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn unsafe_key_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("rpc/keys")).unwrap();
        std::fs::set_permissions(
            dir.path().join("rpc/keys"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            read_key(dir.path(), "launch").unwrap_err().code,
            "unsafe_key_record"
        );
    }
}
