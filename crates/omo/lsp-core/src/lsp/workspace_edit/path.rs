//! Port of TS `workspace-edit-path.ts`.

use super::types::WorkspaceSnapshotEntry;
use crate::request_context::resolve;
use percent_encoding::percent_decode_str;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;

/// TS `WorkspacePathResult` success arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePath {
    pub path: String,
    pub requested_path: String,
    pub followed_symbolic_link: bool,
}

pub type WorkspacePathResult = Result<WorkspacePath, String>;

/// TS `isPathInsideWorkspace` (lexical).
pub fn is_path_inside_workspace(file_path: &str, workspace_root: &str) -> bool {
    let file = crate::request_context::strip_verbatim_prefix(file_path);
    let root = crate::request_context::strip_verbatim_prefix(workspace_root);
    #[cfg(windows)]
    {
        if Path::new(&file).starts_with(&root) {
            return true;
        }
        Path::new(&file.to_lowercase()).starts_with(Path::new(&root.to_lowercase()))
    }
    #[cfg(not(windows))]
    {
        Path::new(&file).starts_with(&root)
    }
}

fn is_symlink(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn canonicalize_missing_path(file_path: &str) -> Result<String, String> {
    let mut ancestor = PathBuf::from(file_path);
    while !ancestor.exists() {
        if !ancestor.pop() {
            return Err(format!("no existing ancestor: {file_path}"));
        }
    }
    let canonical = std::fs::canonicalize(&ancestor).map_err(|error| error.to_string())?;
    let cleaned = crate::request_context::strip_verbatim_prefix(&canonical.to_string_lossy());
    let suffix = Path::new(file_path)
        .strip_prefix(&ancestor)
        .map_err(|error| error.to_string())?;
    Ok(resolve(&Path::new(&cleaned).join(suffix).to_string_lossy()))
}

/// TS `canonicalWorkspaceRoot`.
pub fn canonical_workspace_root(workspace_root: &str) -> WorkspacePathResult {
    let requested = resolve(workspace_root);
    let canonical = std::fs::canonicalize(&requested)
        .map(|p| crate::request_context::strip_verbatim_prefix(&p.to_string_lossy()))
        .map_err(|error| format!("workspace root {workspace_root}: {error}"))?;
    if !Path::new(&canonical).is_dir() {
        return Err(format!(
            "workspace root is not a directory: {workspace_root}"
        ));
    }
    Ok(WorkspacePath {
        path: canonical,
        followed_symbolic_link: is_symlink(&requested),
        requested_path: requested,
    })
}

/// Node `fileURLToPath` for POSIX paths, including its "URI malformed" decode failure.
fn file_url_to_path(parsed: &url::Url) -> Result<String, String> {
    match parsed.host_str() {
        None | Some("") | Some("localhost") => {}
        Some(_) => {
            return Err(format!(
                "File URL host must be \"localhost\" or empty on {}",
                std::env::consts::OS
            ));
        }
    }
    let raw = parsed.path();
    let lower = raw.to_ascii_lowercase();
    if lower.contains("%2f") {
        return Err("File URL path must not include encoded / characters".to_string());
    }
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let valid = bytes.len() > index + 2
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit();
            if !valid {
                return Err("URI malformed".to_string());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    let decoded = percent_decode_str(raw)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| "URI malformed".to_string())?;
    if cfg!(windows) {
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
            return Ok(decoded[1..].to_string());
        }
    }
    Ok(decoded)
}

/// TS `uriToCanonicalWorkspacePath`.
pub fn uri_to_canonical_workspace_path(uri: &str, workspace_root: &str) -> WorkspacePathResult {
    let parsed = url::Url::parse(uri).map_err(|error| format!("non-file URI {uri}: {error}"))?;
    let has_search = parsed.query().is_some_and(|query| !query.is_empty());
    let has_hash = parsed
        .fragment()
        .is_some_and(|fragment| !fragment.is_empty());
    if parsed.scheme() != "file" || has_search || has_hash {
        return Err(format!("non-file URI {uri}"));
    }
    let requested_path = resolve(
        &file_url_to_path(&parsed).map_err(|detail| format!("non-file URI {uri}: {detail}"))?,
    );

    let canonical = if Path::new(&requested_path).exists() {
        std::fs::canonicalize(&requested_path)
            .map(|path| crate::request_context::strip_verbatim_prefix(&path.to_string_lossy()))
            .map_err(|error| format!("{requested_path}: {error}"))?
    } else {
        canonicalize_missing_path(&requested_path)
            .map_err(|detail| format!("{requested_path}: {detail}"))?
    };
    if !is_path_inside_workspace(&canonical, workspace_root) {
        return Err(format!(
            "{requested_path}: outside workspace {workspace_root}"
        ));
    }
    Ok(WorkspacePath {
        path: canonical,
        followed_symbolic_link: Path::new(&requested_path).exists() && is_symlink(&requested_path),
        requested_path,
    })
}

/// Length plus sha256 of a snapshotted file, used where the content itself is not needed.
pub fn file_digest(content: &str) -> String {
    format!(
        "{}:{}",
        content.len(),
        hex::encode(Sha256::digest(content.as_bytes()))
    )
}

/// TS `snapshotPath`.
pub fn snapshot_path(path: &str, include_children: bool) -> Result<WorkspaceSnapshotEntry, String> {
    if !Path::new(path).exists() {
        return Ok(WorkspaceSnapshotEntry::Missing);
    }
    let meta = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if meta.is_file() {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        return Ok(WorkspaceSnapshotEntry::File {
            content: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    if meta.is_dir() {
        if !include_children {
            return Ok(WorkspaceSnapshotEntry::Directory { children: None });
        }
        return Ok(WorkspaceSnapshotEntry::Directory {
            children: Some(read_dir_sorted(path).map_err(|error| error.to_string())?),
        });
    }
    Err(format!("unsupported filesystem entry: {path}"))
}

pub(crate) fn read_dir_sorted(path: &str) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}
