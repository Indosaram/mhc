use std::{collections::HashMap, fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};
use sha2::{Digest, Sha256};

const SALT_BYTES: usize = 32;
static SALTS: OnceLock<Mutex<HashMap<PathBuf, [u8; SALT_BYTES]>>> = OnceLock::new();

fn random_salt() -> [u8; SALT_BYTES] {
    // UUID v4 uses the operating system CSPRNG. Hashing three UUIDs restores
    // a full 256-bit key despite the reserved version/variant bits.
    let mut hash = Sha256::new();
    for _ in 0..3 { hash.update(uuid::Uuid::new_v4().as_bytes()); }
    hash.finalize().into()
}

fn private_file(path: &Path, salt: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(salt)
}

fn read_or_create_salt(path: &Path) -> std::io::Result<Vec<u8>> {
    match fs::metadata(path) {
        Ok(metadata) => {
            let stored = fs::read(path)?;
            if stored.len() == SALT_BYTES { return Ok(stored); }
            #[cfg(unix)] {
                use std::os::unix::fs::MetadataExt;
                if fs::metadata(path)?.ino() != metadata.ino() { return fs::read(path); }
            }
            #[cfg(not(unix))] let _ = metadata;
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let parent = path.parent().expect("salt parent");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)] {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    let staged = path.with_extension(format!("{}.{}", std::process::id(), uuid::Uuid::new_v4()));
    let salt = random_salt();
    let result = (|| {
        private_file(&staged, &salt)?;
        match fs::hard_link(&staged, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported || error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(38) => private_file(path, &salt),
            Err(error) => Err(error),
        }
    })();
    // Cleanup is best effort; the final salt is already atomically published.
    let _ = fs::remove_file(staged);
    match result {
        Ok(()) => fs::read(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => fs::read(path),
        Err(error) => Err(error),
    }
}

fn install_salt(agent_dir: &Path) -> [u8; SALT_BYTES] {
    let mut salts = SALTS.get_or_init(|| Mutex::new(HashMap::new())).lock().expect("seat salts");
    *salts.entry(agent_dir.to_owned()).or_insert_with(|| {
        read_or_create_salt(&agent_dir.join("anthropic-subscription/seat-digest-salt"))
            .ok().and_then(|stored| stored.try_into().ok()).unwrap_or_else(random_salt)
    })
}

/// HMAC-SHA256 with a persisted, private install key; only eight hex digits escape.
pub fn seat_digest(account_name: &str, agent_dir: Option<&Path>) -> String {
    let default_dir;
    let directory = match agent_dir {
        Some(directory) => directory,
        None => { default_dir = PathBuf::from(maho_core::config::get_agent_dir()); &default_dir }
    };
    let salt = install_salt(directory);
    let mut inner_pad = [0x36; 64];
    let mut outer_pad = [0x5c; 64];
    for (index, byte) in salt.iter().enumerate() { inner_pad[index] ^= byte; outer_pad[index] ^= byte; }
    let mut inner = Sha256::new(); inner.update(inner_pad); inner.update(account_name.as_bytes());
    let mut outer = Sha256::new(); outer.update(outer_pad); outer.update(inner.finalize());
    format!("{:x}", outer.finalize())[..8].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_is_stable_private_and_install_scoped() {
        let a = tempfile::tempdir().expect("directory");
        let b = tempfile::tempdir().expect("directory");
        let digest = seat_digest("person@example.com", Some(a.path()));
        assert_eq!(digest.len(), 8);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(digest, seat_digest("person@example.com", Some(a.path())));
        assert_ne!(digest, seat_digest("other", Some(a.path())));
        assert_ne!(digest, seat_digest("person@example.com", Some(b.path())));
        let path = a.path().join("anthropic-subscription/seat-digest-salt");
        assert_eq!(fs::read(&path).expect("salt").len(), 32);
        SALTS.get().expect("cache").lock().expect("cache").remove(a.path());
        assert_eq!(digest, seat_digest("person@example.com", Some(a.path())));
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).expect("metadata").permissions().mode() & 0o777, 0o600);
        }
    }
    #[test]
    fn damaged_salt_is_replaced_and_unwritable_salt_falls_back() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("anthropic-subscription/seat-digest-salt");
        fs::create_dir_all(path.parent().expect("parent")).expect("parent");
        fs::write(&path, b"short").expect("damaged");
        let digest = seat_digest("env", Some(directory.path()));
        assert_eq!(fs::read(path).expect("salt").len(), 32);
        assert_eq!(digest, seat_digest("env", Some(directory.path())));
        let blocked = directory.path().join("file"); fs::write(&blocked, b"x").expect("file");
        assert_eq!(seat_digest("env", Some(&blocked)), seat_digest("env", Some(&blocked)));
    }
    #[test]
    fn simultaneous_creators_converge_without_partial_files() {
        let directory=tempfile::tempdir().expect("directory");
        let path=directory.path().join("anthropic-subscription/seat-digest-salt");
        let barrier=std::sync::Arc::new(std::sync::Barrier::new(12));
        let workers:Vec<_>=(0..12).map(|_| {
            let barrier=barrier.clone(); let path=path.clone();
            std::thread::spawn(move || {barrier.wait();read_or_create_salt(&path).expect("salt")})
        }).collect();
        let salts:Vec<_>=workers.into_iter().map(|worker|worker.join().expect("worker")).collect();
        assert!(salts.iter().all(|salt|salt.len()==32 && salt==&salts[0]));
        assert_eq!(fs::read_dir(path.parent().expect("parent")).expect("entries").count(),1);
    }
    #[test]
    fn hmac_matches_known_key() {
        let directory = tempfile::tempdir().expect("directory");
        SALTS.get_or_init(|| Mutex::new(HashMap::new())).lock().expect("cache").insert(directory.path().to_owned(), [0; 32]);
        // HMAC-SHA256(key=32 zero bytes, message="test").
        assert_eq!(seat_digest("test", Some(directory.path())), "43b0cef9");
    }
}
