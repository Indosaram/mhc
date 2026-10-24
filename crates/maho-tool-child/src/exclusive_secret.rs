//! senpi `tool-child/exclusive-secret.ts`.
use std::{fs::OpenOptions, io, io::Write, path::Path};

use crate::rpc_socket::SOCKET_SECRET_BYTES;

/// The file a create made: a withdrawal removes only the entry that is still this file.
/// (`dev`/`ino` are always 0 on Windows, where `std` exposes no stable file identity and no fallback root is used.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

/// The `O_NOFOLLOW` flag, or 0 where the platform has none (upstream `constants.O_NOFOLLOW ?? 0`).
#[cfg(unix)]
pub(crate) const O_NOFOLLOW: i32 = libc::O_NOFOLLOW;

#[cfg(unix)]
pub(crate) fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(not(unix))]
pub(crate) fn file_identity(_metadata: &std::fs::Metadata) -> FileIdentity {
    FileIdentity { dev: 0, ino: 0 }
}

/// Writes a fresh 0600 secret as a NEW file at `path`. `O_EXCL` with `O_NOFOLLOW` means nothing that already sits at that
/// name - a file, or a symlink to one - is ever followed, truncated or reused: the open fails with EEXIST (or ELOOP)
/// instead. Nothing is removed here: `on_created` names the created file as soon as it exists, so the caller can withdraw
/// it (through a root it has proven again) whatever fails afterwards. The one gap is an `fstat` that fails right after the
/// create: that file is still empty, and it is left where it is.
pub async fn write_exclusive_secret(
    path: &Path,
    on_created: &mut (dyn FnMut(FileIdentity) + Send),
) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    let mut secret = [0u8; SOCKET_SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|error| io::Error::other(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NOFOLLOW).mode(0o600);
    }
    #[cfg(test)]
    crate::socket_root::test_window::before_secret_create(path)?;
    let mut file = options.open(path)?;
    #[cfg(test)]
    crate::socket_root::test_window::record_secret(path);
    let metadata = file.metadata()?;
    on_created(file_identity(&metadata));
    file.write_all(&secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(secret)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    #[tokio::test]
    async fn creates_a_new_0600_file_and_names_it_before_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plain-0.secret");
        let mut created = None;
        let secret = write_exclusive_secret(&path, &mut |identity| created = Some(identity))
            .await
            .expect("secret");
        let metadata = std::fs::symlink_metadata(&path).expect("stat");
        assert_eq!(std::fs::read(&path).expect("read"), secret);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            created,
            Some(FileIdentity {
                dev: metadata.dev(),
                ino: metadata.ino()
            })
        );
    }

    #[tokio::test]
    async fn never_follows_or_truncates_what_already_sits_at_the_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep me").expect("write");
        let link = dir.path().join("link.secret");
        std::os::unix::fs::symlink(&victim, &link).expect("symlink");
        let error = write_exclusive_secret(&link, &mut |_| {})
            .await
            .expect_err("symlink refused");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&victim).expect("read"), "keep me");
        let plain = dir.path().join("plain.secret");
        std::fs::write(&plain, "old").expect("write");
        assert!(write_exclusive_secret(&plain, &mut |_| {}).await.is_err());
        assert_eq!(std::fs::read_to_string(&plain).expect("read"), "old");
    }
}
