//! Rust port of `packages/telemetry-core/src/install-id.ts`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;

use sha2::Digest;
use sha2::Sha256;

pub const TELEMETRY_INSTALL_ID_FILE_NAME: &str = "install-id";

fn fallback_install_ids() -> &'static Mutex<HashMap<String, String>> {
    static IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_install_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The distinct id of a daily-active ping: sha256 of a random 64-hex id stored once in the product's
/// telemetry state dir. The id is derived from nothing on the machine; only its hash leaves the
/// process. Call it only on a send path that has already passed every consent check, because the
/// first call creates the file.
pub fn get_telemetry_install_distinct_id(state_dir: &Path) -> String {
    let id = read_or_create_install_id(&state_dir.join(TELEMETRY_INSTALL_ID_FILE_NAME));
    hex::encode(Sha256::digest(id.as_bytes()))
}

fn mint_install_id() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn open_options(create_new: bool) -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn write_install_id(path: &Path, install_id: &str, create_new: bool) -> std::io::Result<()> {
    let mut file = open_options(create_new).open(path)?;
    file.write_all(format!("{install_id}\n").as_bytes())
}

fn read_or_create_install_id(path: &Path) -> String {
    if let Some(persisted) = read_install_id(path) {
        return persisted;
    }
    let install_id = mint_install_id();
    let key = path.to_string_lossy().into_owned();
    let created = path
        .parent()
        .is_none_or(|dir| fs::create_dir_all(dir).is_ok())
        && write_install_id(path, &install_id, true).is_ok();
    if created {
        forget_fallback(&key);
        return install_id;
    }
    // A concurrent writer may have created it first; its id wins. A malformed file is repaired, and a
    // read-only state dir keeps one id per process.
    if let Some(concurrently_created) = read_install_id(path) {
        return concurrently_created;
    }
    if write_install_id(path, &install_id, false).is_ok() {
        forget_fallback(&key);
        return install_id;
    }
    let mut fallbacks = fallback_install_ids()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    fallbacks.entry(key).or_insert(install_id).clone()
}

fn forget_fallback(key: &str) {
    fallback_install_ids()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(key);
}

fn read_install_id(path: &Path) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    is_install_id(value).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_one_hashed_id_and_repairs_malformed_file() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let first = get_telemetry_install_distinct_id(dir.path());
        assert_eq!(first.len(), 64);
        assert_eq!(get_telemetry_install_distinct_id(dir.path()), first);
        let path = dir.path().join(TELEMETRY_INSTALL_ID_FILE_NAME);
        let raw = fs::read_to_string(&path)?;
        assert!(is_install_id(raw.trim()));
        assert_ne!(raw.trim(), first);
        assert_eq!(first, hex::encode(Sha256::digest(raw.trim().as_bytes())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        }
        fs::write(&path, "garbage")?;
        let repaired = get_telemetry_install_distinct_id(dir.path());
        assert_ne!(repaired, first);
        let repaired_raw = fs::read_to_string(&path)?;
        assert!(is_install_id(repaired_raw.trim()));
        assert_eq!(
            repaired,
            hex::encode(Sha256::digest(repaired_raw.trim().as_bytes()))
        );
        assert_eq!(get_telemetry_install_distinct_id(dir.path()), repaired);
        Ok(())
    }

    #[test]
    fn unwritable_dir_keeps_one_id_per_process() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x")?;
        let state = blocker.join("state");
        let first = get_telemetry_install_distinct_id(&state);
        assert_eq!(get_telemetry_install_distinct_id(&state), first);
        Ok(())
    }
}
