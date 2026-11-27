//! Port of senpi `packages/coding-agent/src/modes/rpc/project-file-path.ts`.
//!
//! Which workspace files `read_project_file` / `write_project_file` may touch, and where they resolve
//! on disk.

use std::path::{Path, PathBuf};

/// Under the host's 1,000,000-character message limit, so an oversized write reaches this typed refusal.
pub const PROJECT_FILE_MAX_BYTES: usize = 512 * 1024;

const INSTRUCTION_FILE_NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const EXCLUDED_SEGMENTS: [&str; 2] = [".git", "node_modules"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFilePath {
    pub relative: String,
    pub segments: Vec<String>,
}

pub fn parse_project_file_path(input: &str) -> Option<ProjectFilePath> {
    if input.is_empty() || input.starts_with('/') || input.contains(['\\', ':', '\0']) {
        return None;
    }
    let segments: Vec<String> = input.split('/').map(str::to_owned).collect();
    for segment in &segments {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || EXCLUDED_SEGMENTS.contains(&segment.as_str())
        {
            return None;
        }
    }
    let name = segments.last().map(String::as_str).unwrap_or("");
    let is_rule = segments.len() >= 3 && segments[0] == ".omo" && segments[1] == "rules";
    let is_omo_config = segments.len() == 2 && segments[0] == ".omo" && name == "omo.jsonc";
    if !is_rule && !is_omo_config && !INSTRUCTION_FILE_NAMES.contains(&name) {
        return None;
    }
    Some(ProjectFilePath {
        relative: segments.join("/"),
        segments,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProjectFile {
    Ok {
        absolute: PathBuf,
        exists: bool,
        missing_folders: Vec<PathBuf>,
    },
    Refused,
}

fn is_missing_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error() == Some(libc::ENOTDIR)
}

async fn lstat_or_missing(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => Ok(Some(meta)),
        Err(err) if is_missing_error(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Resolves `file` inside the real workspace root; `missing_folders` are the parent folders a write
/// must create.
pub async fn resolve_project_file(
    cwd: &Path,
    file: &ProjectFilePath,
) -> std::io::Result<ResolvedProjectFile> {
    let mut current = maho_core::paths::strip_verbatim_prefix(tokio::fs::canonicalize(cwd).await?);
    let mut missing_folders = Vec::new();
    let folder_count = file.segments.len().saturating_sub(1);
    for folder in &file.segments[..folder_count] {
        current = current.join(folder);
        if !missing_folders.is_empty() {
            missing_folders.push(current.clone());
            continue;
        }
        let Some(stat) = lstat_or_missing(&current).await? else {
            missing_folders.push(current.clone());
            continue;
        };
        if stat.file_type().is_symlink() || !stat.is_dir() {
            return Ok(ResolvedProjectFile::Refused);
        }
    }
    let file_name = file.segments.last().map(String::as_str).unwrap_or("");
    let absolute = current.join(file_name);
    if !missing_folders.is_empty() {
        return Ok(ResolvedProjectFile::Ok {
            absolute,
            exists: false,
            missing_folders,
        });
    }
    let Some(stat) = lstat_or_missing(&absolute).await? else {
        return Ok(ResolvedProjectFile::Ok {
            absolute,
            exists: false,
            missing_folders,
        });
    };
    if stat.file_type().is_symlink() || !stat.is_file() {
        return Ok(ResolvedProjectFile::Refused);
    }
    Ok(ResolvedProjectFile::Ok {
        absolute,
        exists: true,
        missing_folders,
    })
}

/// Creates the missing parent folders one at a time; a non-recursive `mkdir` fails on anything that
/// appeared in the meantime, and each new folder is re-checked as a real folder before the next one
/// is made inside it.
pub async fn create_missing_folders(folders: &[PathBuf]) -> std::io::Result<bool> {
    for folder in folders {
        match tokio::fs::create_dir(folder).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        let stat = tokio::fs::symlink_metadata(folder).await?;
        if stat.file_type().is_symlink() || !stat.is_dir() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_file_path_allowlist_and_exclusions() {
        assert!(parse_project_file_path("AGENTS.md").is_some());
        assert!(parse_project_file_path("packages/app/CLAUDE.md").is_some());
        assert!(parse_project_file_path(".omo/rules/style.md").is_some());
        assert!(parse_project_file_path(".omo/rules/sub/a.md").is_some());
        assert!(parse_project_file_path(".omo/omo.jsonc").is_some());

        assert!(parse_project_file_path("").is_none());
        assert!(parse_project_file_path("/AGENTS.md").is_none());
        assert!(parse_project_file_path("../AGENTS.md").is_none());
        assert!(parse_project_file_path(".git/AGENTS.md").is_none());
        assert!(parse_project_file_path("node_modules/pkg/CLAUDE.md").is_none());
        assert!(parse_project_file_path(".omo/rules").is_none());
        assert!(parse_project_file_path("sub/.omo/omo.jsonc").is_none());
        assert!(parse_project_file_path("README.md").is_none());
        assert!(parse_project_file_path("a\\AGENTS.md").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_project_file_refuses_symlinks_and_tracks_missing_folders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let link = dir.path().join(".omo");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
        let rule = parse_project_file_path(".omo/rules/r.md").expect("rule");
        assert_eq!(
            resolve_project_file(dir.path(), &rule)
                .await
                .expect("resolve"),
            ResolvedProjectFile::Refused
        );

        let nested = parse_project_file_path("sub/dir/AGENTS.md").expect("nested");
        let ResolvedProjectFile::Ok {
            exists,
            missing_folders,
            ..
        } = resolve_project_file(dir.path(), &nested)
            .await
            .expect("resolve")
        else {
            panic!("expected Ok");
        };
        assert!(!exists);
        assert_eq!(missing_folders.len(), 2);
        assert!(
            create_missing_folders(&missing_folders)
                .await
                .expect("mkdir")
        );
        assert!(dir.path().join("sub/dir").is_dir());
    }
}
