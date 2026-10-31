//! Port of senpi packages/coding-agent/src/core/config-reference-file.ts.
//!
//! A `{"$file": ...}` reference is read only after walking the original spelling and every symlink target:
//! each entry must be owned by root or the current user, no writable-by-others ancestor may hold a
//! writable-by-others entry (a sticky directory excepted), the final file must be a regular file at most
//! 65,536 bytes, the opened file must be the walked one (dev/ino), and its UTF-8 text is trimmed.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use maho_ai::utils::config_reference_error::{ConfigReferenceError, ConfigReferenceReason};
use serde_json::{Value, json};

const MAX_BYTES: u64 = 65_536;

static READABLE_WARNINGS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub struct FileReference<'a> {
    pub field: &'a str,
    /// The declared `$file` path.
    pub file: &'a str,
    pub base_dir: &'a Path,
}

impl FileReference<'_> {
    fn fail(&self, reason: ConfigReferenceReason) -> ConfigReferenceError {
        ConfigReferenceError::new(self.field, json!({ "$file": self.file }), reason)
    }
}

/// `~/` names the home directory; any other relative path is relative to the declaring config.
fn absolute_path(input: &FileReference<'_>) -> PathBuf {
    let declared = input.file;
    let from_home = declared.starts_with("~/") || (cfg!(windows) && declared.starts_with("~\\"));
    if from_home {
        return crate::config::home_dir_path().join(&declared[2..]);
    }
    let path = Path::new(declared);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        input.base_dir.join(path)
    }
}

#[cfg(unix)]
mod meta {
    use std::os::unix::fs::MetadataExt;

    pub fn uid(meta: &std::fs::Metadata) -> u32 {
        meta.uid()
    }
    pub fn mode(meta: &std::fs::Metadata) -> u32 {
        meta.mode()
    }
    pub fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    pub fn current_uid() -> u32 {
        nix::unistd::getuid().as_raw()
    }
}

#[cfg(not(unix))]
mod meta {
    pub fn uid(_: &std::fs::Metadata) -> u32 {
        0
    }
    pub fn mode(_: &std::fs::Metadata) -> u32 {
        0
    }
    pub fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
        left.len() == right.len() && left.modified().ok() == right.modified().ok()
    }
    pub fn current_uid() -> u32 {
        0
    }
}

fn check_owner(
    input: &FileReference<'_>,
    stat: &std::fs::Metadata,
) -> Result<(), ConfigReferenceError> {
    if cfg!(unix) && meta::uid(stat) != 0 && meta::uid(stat) != meta::current_uid() {
        return Err(input.fail(ConfigReferenceReason::UnsafeOwner));
    }
    Ok(())
}

fn check_writable(
    input: &FileReference<'_>,
    stat: &std::fs::Metadata,
    ancestor: bool,
) -> Result<(), ConfigReferenceError> {
    if !cfg!(unix) || stat.file_type().is_symlink() || meta::mode(stat) & 0o022 == 0 {
        return Ok(());
    }
    if ancestor && stat.is_dir() && meta::mode(stat) & 0o1000 != 0 {
        return Ok(());
    }
    Err(input.fail(ConfigReferenceReason::UnsafePermissions))
}

fn io_reason(error: &std::io::Error) -> ConfigReferenceReason {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            ConfigReferenceReason::Missing
        }
        _ => ConfigReferenceReason::Unreadable,
    }
}

fn split(path: &Path) -> (PathBuf, Vec<String>) {
    let mut root = PathBuf::new();
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => root.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_owned()),
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
        }
    }
    (root, parts)
}

/// Walk the original spelling and every symlink target before opening any bytes.
fn walk(input: &FileReference<'_>) -> Result<(PathBuf, std::fs::Metadata), ConfigReferenceError> {
    let lstat = |path: &Path| {
        std::fs::symlink_metadata(path).map_err(|error| input.fail(io_reason(&error)))
    };
    let absolute = absolute_path(input);
    let (mut current, pending) = split(&absolute);
    let mut pending: std::collections::VecDeque<String> = pending.into();
    let mut stat = lstat(&current)?;
    let mut links = 0;
    check_owner(input, &stat)?;
    check_writable(input, &stat, true)?;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if !stat.is_dir() {
            return Err(input.fail(ConfigReferenceReason::NonRegular));
        }
        let next = if part == ".." {
            current
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| current.clone())
        } else {
            current.join(&part)
        };
        let next_stat = lstat(&next)?;
        check_owner(input, &next_stat)?;
        if cfg!(unix)
            && meta::mode(&stat) & 0o022 != 0
            && !next_stat.file_type().is_symlink()
            && meta::mode(&next_stat) & 0o022 != 0
        {
            return Err(input.fail(ConfigReferenceReason::UnsafePermissions));
        }
        check_writable(input, &next_stat, !pending.is_empty())?;
        // A sticky writable parent is safe only while its next entry cannot be modified by another user;
        // check_owner and check_writable enforce that.
        if next_stat.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return Err(input.fail(ConfigReferenceReason::Unreadable));
            }
            let target =
                std::fs::read_link(&next).map_err(|error| input.fail(io_reason(&error)))?;
            if !meta::same_file(&next_stat, &lstat(&next)?) {
                return Err(input.fail(ConfigReferenceReason::Changed));
            }
            let target_path = if target.is_absolute() {
                target
            } else {
                current.join(target)
            };
            let (root, parts) = split(&target_path);
            current = root;
            for part in parts.into_iter().rev() {
                pending.push_front(part);
            }
            stat = lstat(&current)?;
            check_owner(input, &stat)?;
            check_writable(input, &stat, true)?;
        } else {
            current = next;
            stat = next_stat;
        }
    }
    if !stat.is_file() {
        return Err(input.fail(ConfigReferenceReason::NonRegular));
    }
    Ok((current, stat))
}

fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    options.open(path)
}

/// senpi `readConfigReferenceFile`.
pub fn read_config_reference_file(
    input: &FileReference<'_>,
) -> Result<String, ConfigReferenceError> {
    use std::io::Read;
    let (path, walked) = walk(input)?;
    let file = open_no_follow(&path).map_err(|error| input.fail(io_reason(&error)))?;
    let stat = file
        .metadata()
        .map_err(|error| input.fail(io_reason(&error)))?;
    if !meta::same_file(&walked, &stat) {
        return Err(input.fail(ConfigReferenceReason::Changed));
    }
    if !stat.is_file() {
        return Err(input.fail(ConfigReferenceReason::NonRegular));
    }
    check_owner(input, &stat)?;
    check_writable(input, &stat, false)?;
    if stat.len() > MAX_BYTES {
        return Err(input.fail(ConfigReferenceReason::Oversize));
    }
    let mut buffer = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|error| input.fail(io_reason(&error)))?;
    if buffer.len() as u64 > MAX_BYTES {
        return Err(input.fail(ConfigReferenceReason::Oversize));
    }
    let text =
        String::from_utf8(buffer).map_err(|_| input.fail(ConfigReferenceReason::InvalidUtf8))?;
    // JS `String.prototype.trim`: Unicode white space and line terminators.
    let value = text.trim().to_owned();
    if value.is_empty() {
        return Err(input.fail(ConfigReferenceReason::Empty));
    }
    if cfg!(unix)
        && meta::mode(&stat) & 0o044 != 0
        && READABLE_WARNINGS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.clone())
    {
        crate::output_guard::maho_write_stderr(&format!(
            "{}: reference file is readable by other users\n",
            input.field
        ));
    }
    Ok(value)
}

/// The declared `$file` reference as JSON (for errors raised outside this module).
pub fn file_reference_value(file: &str) -> Value {
    json!({ "$file": file })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn read(dir: &Path, file: &str) -> Result<String, ConfigReferenceReason> {
        read_config_reference_file(&FileReference {
            field: "providers.p.apiKey",
            file,
            base_dir: dir,
        })
        .map_err(|error| error.reason)
    }

    /// senpi test/suite/regressions/3858-reference-files.test.ts.
    #[test]
    fn reads_a_private_file_relative_to_its_config_and_trims_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("cfg");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        std::fs::write(dir.join("key"), "  secret-value\n").expect("write");
        std::fs::set_permissions(dir.join("key"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        assert_eq!(read(&dir, "key").as_deref(), Ok("secret-value"));
        assert_eq!(read(&dir, "missing"), Err(ConfigReferenceReason::Missing));
    }

    #[test]
    fn refuses_writable_oversize_empty_and_non_regular_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        std::fs::write(dir.join("open"), "value").expect("write");
        std::fs::set_permissions(dir.join("open"), std::fs::Permissions::from_mode(0o666))
            .expect("chmod");
        assert_eq!(
            read(&dir, "open"),
            Err(ConfigReferenceReason::UnsafePermissions)
        );
        std::fs::write(dir.join("big"), vec![b'a'; 65_537]).expect("write");
        std::fs::set_permissions(dir.join("big"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        assert_eq!(read(&dir, "big"), Err(ConfigReferenceReason::Oversize));
        std::fs::write(dir.join("blank"), " \n").expect("write");
        std::fs::set_permissions(dir.join("blank"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        assert_eq!(read(&dir, "blank"), Err(ConfigReferenceReason::Empty));
        std::fs::create_dir(dir.join("sub")).expect("mkdir");
        assert_eq!(read(&dir, "sub"), Err(ConfigReferenceReason::NonRegular));
        std::fs::write(dir.join("bad"), [0xff, 0xfe]).expect("write");
        std::fs::set_permissions(dir.join("bad"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        assert_eq!(read(&dir, "bad"), Err(ConfigReferenceReason::InvalidUtf8));
    }

    #[test]
    fn follows_a_symlink_to_a_private_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        std::fs::write(dir.join("target"), "linked").expect("write");
        std::fs::set_permissions(dir.join("target"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        std::os::unix::fs::symlink(dir.join("target"), dir.join("link")).expect("symlink");
        assert_eq!(read(&dir, "link").as_deref(), Ok("linked"));
    }
}
