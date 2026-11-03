//! Where a session keeps the images its tools produced (senpi `rpc/tool-media-store.ts`).
//!
//! A `media_placeholders` client never receives the bytes on the socket. The host writes them here, once,
//! BEFORE the placeholder that names the file is emitted, so a client can render the picture from the path
//! whenever it likes. The directory sits next to the session files, never under the user's project, and is
//! removed with the durable session.
use crate::media_placeholders::base64_byte_length;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolMediaScope {
    pub session_dir: PathBuf,
    pub durable_session_id: String,
}

pub const MAX_TOOL_IMAGE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_SESSION_MEDIA_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolMediaLimits {
    pub per_image: usize,
    pub per_session: usize,
}

impl Default for ToolMediaLimits {
    fn default() -> Self {
        Self {
            per_image: MAX_TOOL_IMAGE_BYTES,
            per_session: MAX_SESSION_MEDIA_BYTES,
        }
    }
}

/// senpi `PersistedMedia`: where the image lives, or why it does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistedMedia {
    Path(PathBuf),
    Unavailable(MediaUnavailable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaUnavailable {
    ImageTooLarge,
    SessionLimit,
    StorageError,
}

impl MediaUnavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageTooLarge => "image_too_large",
            Self::SessionLimit => "session_limit",
            Self::StorageError => "storage_error",
        }
    }
}

const MEDIA_DIRECTORY: &str = "media";

fn extension(mime_type: &str) -> Option<&'static str> {
    match mime_type.to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn signature_matches(mime_type: &str, bytes: &[u8]) -> bool {
    match mime_type.to_ascii_lowercase().as_str() {
        "image/png" => bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF8"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

/// Bytes already stored per media root: the reservation every concurrent write is checked against.
fn reserved() -> std::sync::MutexGuard<'static, HashMap<PathBuf, usize>> {
    static RESERVED: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    RESERVED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Outcomes per image block, so re-emitting a record (`get_messages`, `turn_end`) never rehashes it.
/// senpi keys the live block object; a block is identified here by its reference and its data's digest.
type MediaOutcomes = HashMap<(ToolMediaScope, String, usize, String), PersistedMedia>;
fn outcomes() -> std::sync::MutexGuard<'static, MediaOutcomes> {
    static OUTCOMES: OnceLock<Mutex<MediaOutcomes>> = OnceLock::new();
    OUTCOMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `<sessionDir>/media/<durableSessionId>`; `None` for an id that is not a valid session id.
pub fn tool_media_root(scope: &ToolMediaScope) -> Option<PathBuf> {
    valid_session_id(&scope.durable_session_id).then(|| {
        scope
            .session_dir
            .join(MEDIA_DIRECTORY)
            .join(&scope.durable_session_id)
    })
}

/// The media scope of a live session: its file's directory and its durable id.
pub fn live_tool_media_scope(
    session_file: Option<&str>,
    durable_session_id: Option<&str>,
) -> Option<ToolMediaScope> {
    let session_file = Path::new(session_file?);
    Some(ToolMediaScope {
        session_dir: session_file.parent()?.to_path_buf(),
        durable_session_id: durable_session_id?.to_owned(),
    })
}

fn assert_real_directory_chain(root: &Path, call_directory: &Path) -> std::io::Result<()> {
    for path in [root.parent().unwrap_or(root), root, call_directory] {
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && !metadata.is_dir()
        {
            return Err(std::io::Error::other(format!(
                "not a real directory: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn directory_bytes(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            _ => entry
                .metadata()
                .map_or(0, |metadata| metadata.len() as usize),
        })
        .sum()
}

fn stored_bytes(reserved: &mut HashMap<PathBuf, usize>, root: &Path) -> usize {
    *reserved.entry(root.to_path_buf()).or_insert_with(|| {
        if root.exists() {
            directory_bytes(root)
        } else {
            0
        }
    })
}

fn write_image(root: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let call_directory = path.parent().unwrap_or(root);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(call_directory)?;
    assert_real_directory_chain(root, call_directory)?;
    let mut suffix = [0u8; 6];
    getrandom::fill(&mut suffix).map_err(|error| std::io::Error::other(error.to_string()))?;
    let staging = PathBuf::from(format!("{}.{}.tmp", path.display(), hex::encode(suffix)));
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&staging)?, bytes)?;
        std::fs::rename(&staging, path)?;
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o400))?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
}

/// Persist one tool-result image and report where it lives, or why it does not.
///
/// Layout: `<sessionDir>/media/<durableSessionId>/<sha256(toolCallId)>/<contentIndex>-<sha256(bytes)>.<ext>`.
/// The content hash makes a file immutable and a repeat a no-op; the limits reject new storage and never
/// evict a file a transcript may still reference.
pub fn persist_tool_image(
    scope: &ToolMediaScope,
    tool_call_id: &str,
    content_index: usize,
    data: &str,
    mime_type: Option<&str>,
    limits: ToolMediaLimits,
) -> PersistedMedia {
    let key = (
        scope.clone(),
        tool_call_id.to_owned(),
        content_index,
        sha256_hex(data.as_bytes()),
    );
    if let Some(known) = outcomes().get(&key) {
        return known.clone();
    }
    let outcome = store(
        scope,
        tool_call_id,
        content_index,
        data,
        mime_type.unwrap_or_default(),
        limits,
    );
    if outcome != PersistedMedia::Unavailable(MediaUnavailable::StorageError) {
        outcomes().insert(key, outcome.clone());
    }
    outcome
}

fn store(
    scope: &ToolMediaScope,
    tool_call_id: &str,
    content_index: usize,
    data: &str,
    mime_type: &str,
    limits: ToolMediaLimits,
) -> PersistedMedia {
    use MediaUnavailable::*;
    let Some(extension) = extension(mime_type) else {
        return PersistedMedia::Unavailable(StorageError);
    };
    if base64_byte_length(data) > limits.per_image {
        return PersistedMedia::Unavailable(ImageTooLarge);
    }
    let bytes = maho_core::utils::image_convert::decode_base64(data);
    if bytes.len() > limits.per_image {
        return PersistedMedia::Unavailable(ImageTooLarge);
    }
    if !signature_matches(mime_type, &bytes) {
        return PersistedMedia::Unavailable(StorageError);
    }
    let Some(root) = tool_media_root(scope) else {
        return PersistedMedia::Unavailable(StorageError);
    };
    let path = root.join(sha256_hex(tool_call_id.as_bytes())).join(format!(
        "{content_index}-{}.{extension}",
        sha256_hex(&bytes)
    ));
    if assert_real_directory_chain(&root, path.parent().unwrap_or(&root)).is_err() {
        return PersistedMedia::Unavailable(StorageError);
    }
    if path.exists() {
        return PersistedMedia::Path(path);
    }
    {
        let mut reserved = reserved();
        let used = stored_bytes(&mut reserved, &root);
        if used + bytes.len() > limits.per_session {
            return PersistedMedia::Unavailable(SessionLimit);
        }
        reserved.insert(root.clone(), used + bytes.len());
    }
    if write_image(&root, &path, &bytes).is_err() {
        let mut reserved = reserved();
        let used = stored_bytes(&mut reserved, &root);
        reserved.insert(root, used.saturating_sub(bytes.len()));
        return PersistedMedia::Unavailable(StorageError);
    }
    PersistedMedia::Path(path)
}

/// senpi `toolMediaPersister`: a persister bound to a scope read at each call; `None` while no scope is known.
pub fn tool_media_persister(
    scope: impl Fn() -> Option<ToolMediaScope> + Send + Sync,
) -> impl Fn(&str, usize, &str, Option<&str>) -> Option<PersistedMedia> + Send + Sync {
    move |tool_call_id, content_index, data, mime_type| {
        let current = scope()?;
        Some(persist_tool_image(
            &current,
            tool_call_id,
            content_index,
            data,
            mime_type,
            ToolMediaLimits::default(),
        ))
    }
}

pub fn remove_tool_media(scope: &ToolMediaScope) -> std::io::Result<()> {
    let Some(root) = tool_media_root(scope) else {
        return Err(std::io::Error::other("invalid session id"));
    };
    reserved().remove(&root);
    outcomes().retain(|(owner, ..), _| owner != scope);
    make_tree_writable(&root);
    match std::fs::remove_dir_all(&root) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn make_tree_writable(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    #[cfg(unix)]
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => make_tree_writable(&entry.path()),
            #[cfg(unix)]
            Ok(kind) if kind.is_file() => {
                let _ = std::fs::set_permissions(
                    entry.path(),
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                );
            }
            _ => {}
        }
    }
}

/// The media scope of the session stored in `session_file`; a file with no readable header has none.
pub fn tool_media_scope_of_session_file(session_file: &Path) -> Option<ToolMediaScope> {
    use std::io::Read;
    let mut head = vec![0u8; 4096];
    let size = std::fs::File::open(session_file)
        .ok()?
        .read(&mut head)
        .ok()?;
    let text = String::from_utf8_lossy(&head[..size]);
    let first = text.split('\n').next().unwrap_or_default();
    let header: serde_json::Value = serde_json::from_str(first).ok()?;
    Some(ToolMediaScope {
        session_dir: session_file.parent()?.to_path_buf(),
        durable_session_id: header.get("id")?.as_str()?.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // An 8-byte PNG signature plus a payload byte, base64.
    const PNG: &str = "iVBORw0KGgoA";

    fn scope(dir: &Path, id: &str) -> ToolMediaScope {
        ToolMediaScope {
            session_dir: dir.to_path_buf(),
            durable_session_id: id.into(),
        }
    }

    #[test]
    fn images_land_once_under_the_content_addressed_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = scope(dir.path(), "s1");
        let first = persist_tool_image(
            &scope,
            "call",
            0,
            PNG,
            Some("image/png"),
            ToolMediaLimits::default(),
        );
        let PersistedMedia::Path(path) = &first else {
            panic!("not stored: {first:?}")
        };
        let expected = dir.path().join("media/s1").join(sha256_hex(b"call"));
        assert_eq!(path.parent(), Some(expected.as_path()));
        assert!(
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("0-"))
        );
        assert_eq!(
            persist_tool_image(
                &scope,
                "call",
                0,
                PNG,
                Some("image/png"),
                ToolMediaLimits::default()
            ),
            first
        );
        remove_tool_media(&scope).expect("remove");
        assert!(!dir.path().join("media/s1").exists());
    }

    #[test]
    fn wrong_types_signatures_and_limits_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = scope(dir.path(), "s2");
        let limits = ToolMediaLimits::default();
        assert_eq!(
            persist_tool_image(&scope, "c", 0, PNG, Some("image/tiff"), limits),
            PersistedMedia::Unavailable(MediaUnavailable::StorageError)
        );
        assert_eq!(
            persist_tool_image(&scope, "c", 1, PNG, Some("image/jpeg"), limits),
            PersistedMedia::Unavailable(MediaUnavailable::StorageError)
        );
        assert_eq!(
            persist_tool_image(
                &scope,
                "c",
                2,
                PNG,
                Some("image/png"),
                ToolMediaLimits {
                    per_image: 4,
                    per_session: 100
                }
            ),
            PersistedMedia::Unavailable(MediaUnavailable::ImageTooLarge)
        );
        assert_eq!(
            persist_tool_image(
                &scope,
                "c",
                3,
                PNG,
                Some("image/png"),
                ToolMediaLimits {
                    per_image: 100,
                    per_session: 4
                }
            ),
            PersistedMedia::Unavailable(MediaUnavailable::SessionLimit)
        );
        assert_eq!(
            persist_tool_image(
                &ToolMediaScope {
                    session_dir: dir.path().into(),
                    durable_session_id: "../x".into()
                },
                "c",
                0,
                PNG,
                Some("image/png"),
                limits
            ),
            PersistedMedia::Unavailable(MediaUnavailable::StorageError)
        );
    }

    #[test]
    fn scopes_come_from_live_sessions_and_session_headers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("x.jsonl");
        std::fs::write(&file, "{\"type\":\"session\",\"id\":\"d9\"}\n{}").expect("write");
        assert_eq!(
            tool_media_scope_of_session_file(&file),
            Some(scope(dir.path(), "d9"))
        );
        assert_eq!(
            tool_media_scope_of_session_file(&dir.path().join("none")),
            None
        );
        assert_eq!(
            live_tool_media_scope(Some(&file.to_string_lossy()), Some("d9")),
            Some(scope(dir.path(), "d9"))
        );
        assert_eq!(live_tool_media_scope(None, Some("d9")), None);
        let unscoped = tool_media_persister(|| None);
        assert_eq!(unscoped("c", 0, PNG, Some("image/png")), None);
    }
}
