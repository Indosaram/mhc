//! Port of senpi mcp/config-identity-key.ts: a per-installation HMAC key for persisted identities.
//!
//! Identities derived from configuration and credentials are HMAC-SHA-256 under a private 32-byte key in the agent
//! dir, so a persisted cache never holds an offline-guessable plain hash of a secret (senpi#3858). Without a valid
//! key the identity is `ephemeral:` (keyed by a per-process key) and nothing keyed may be persisted.
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

const KEY_FILE: &str = "config-identity.key";
const KEY_BYTES: usize = 32;
const EPHEMERAL_PREFIX: &str = "ephemeral:";

static PROCESS_KEY: LazyLock<[u8; KEY_BYTES]> = LazyLock::new(|| {
    let mut key = [0u8; KEY_BYTES];
    getrandom::fill(&mut key).expect("OS entropy unavailable for the MCP identity key");
    key
});
static INSTALLATION_KEYS: LazyLock<Mutex<HashMap<PathBuf, Option<[u8; KEY_BYTES]>>>> =
    LazyLock::new(Default::default);
static WARNED_PATHS: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

enum KeyRead {
    Valid([u8; KEY_BYTES]),
    Missing,
    Invalid,
}

fn default_agent_dir() -> PathBuf {
    PathBuf::from(maho_core::config::get_agent_dir())
}

/// HMAC-SHA-256 under the installation key; without a valid key, an `ephemeral:` identity nothing may persist.
pub fn keyed_identity(agent_dir: Option<&Path>, payload: &str) -> String {
    let agent_dir = agent_dir.map_or_else(default_agent_dir, Path::to_path_buf);
    match installation_key(&agent_dir) {
        Some(key) => hmac(&key, payload),
        None => format!("{EPHEMERAL_PREFIX}{}", hmac(&*PROCESS_KEY, payload)),
    }
}

pub fn is_ephemeral_identity(identity: Option<&str>) -> bool {
    identity.is_some_and(|identity| identity.starts_with(EPHEMERAL_PREFIX))
}

/// A short keyed fingerprint for warnings and log redactions; never an offline-guessable plain hash.
pub fn keyed_fingerprint(value: &str) -> String {
    let key = installation_key(&default_agent_dir()).unwrap_or(*PROCESS_KEY);
    hmac(&key, value)[..8].to_owned()
}

fn hmac(key: &[u8], payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn installation_key(agent_dir: &Path) -> Option<[u8; KEY_BYTES]> {
    let mut keys = INSTALLATION_KEYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *keys
        .entry(agent_dir.to_path_buf())
        .or_insert_with(|| load_or_publish_key(agent_dir))
}

fn load_or_publish_key(agent_dir: &Path) -> Option<[u8; KEY_BYTES]> {
    let path = agent_dir.join(KEY_FILE);
    match read_key(&path) {
        KeyRead::Valid(key) => Some(key),
        KeyRead::Missing => publish_key(agent_dir, &path),
        KeyRead::Invalid => None,
    }
}

/// Only a regular file we own, mode 0600, of exactly 32 bytes is ever used; anything else is left untouched.
fn read_key(path: &Path) -> KeyRead {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return KeyRead::Missing,
        Err(_) => {
            return invalid_key(
                path,
                "cannot open safely",
                "repair access and ensure it is a regular file, not a symlink",
            );
        }
    };
    let Ok(metadata) = file.metadata() else {
        return invalid_key(
            path,
            "cannot open safely",
            "repair access and ensure it is a regular file, not a symlink",
        );
    };
    if !metadata.is_file() {
        return invalid_key(
            path,
            "not a regular file",
            "restore the original private key file",
        );
    }
    if metadata.len() != KEY_BYTES as u64 {
        return invalid_key(
            path,
            &format!("wrong size ({} bytes)", metadata.len()),
            "restore the original 32-byte key from backup",
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::getuid().as_raw() {
            return invalid_key(
                path,
                "wrong owner",
                "chown the key file to your current user",
            );
        }
        if metadata.mode() & 0o777 != 0o600 {
            return invalid_key(path, "wrong mode", "chmod 600 the key file");
        }
    }
    let mut key = [0u8; KEY_BYTES];
    match file.read_exact(&mut key) {
        Ok(()) => KeyRead::Valid(key),
        Err(_) => invalid_key(
            path,
            "incomplete read",
            "restore the original 32-byte key from backup",
        ),
    }
}

fn invalid_key(path: &Path, problem: &str, repair: &str) -> KeyRead {
    if WARNED_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf())
    {
        maho_ext_api::diagnostic(&format!(
            "MCP identity key {}: {problem}; persistent catalog cache is disabled. To fix: {repair}, then restart. The file was left unchanged.",
            path.display()
        ));
    }
    KeyRead::Invalid
}

/// A private temporary file published with a hard link: the key is complete or absent, and never replaced.
fn publish_key(agent_dir: &Path, path: &Path) -> Option<[u8; KEY_BYTES]> {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).ok()?;
    let temporary = agent_dir.join(format!(
        "{KEY_FILE}.{}.{}.tmp",
        std::process::id(),
        suffix
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    let result = (|| -> std::io::Result<[u8; KEY_BYTES]> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(agent_dir)?;
        let mut key = [0u8; KEY_BYTES];
        getrandom::fill(&mut key).map_err(|error| std::io::Error::other(error.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&key)?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(agent_dir)?.sync_all()?;
        Ok(key)
    })();
    let _ = std::fs::remove_file(&temporary);
    match result {
        Ok(key) => Some(key),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => match read_key(path) {
            KeyRead::Valid(key) => Some(key),
            _ => None,
        },
        Err(_) => None,
    }
}
