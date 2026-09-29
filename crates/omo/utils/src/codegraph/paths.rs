use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegraphWorkspacePaths {
    pub data_dir: PathBuf,
    pub data_root: PathBuf,
    pub project_link: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveCodegraphWorkspacePathsOptions {
    pub home_dir: Option<PathBuf>,
}

pub fn sanitize_base(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut last_was_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            sanitized.push(ch);
            last_was_dash = ch == '-';
        } else if !last_was_dash {
            sanitized.push('-');
            last_was_dash = true;
        }
    }
    if sanitized.is_empty() {
        "workspace".to_string()
    } else {
        sanitized
    }
}

pub fn codegraph_data_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".maho").join("codegraph")
}

pub fn canonicalize_codegraph_path(path: &Path) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    match std::fs::canonicalize(&resolved) {
        Ok(canonical) => {
            #[cfg(windows)]
            {
                let s = canonical.to_string_lossy();
                if let Some(stripped) = s.strip_prefix(r"\\?\") {
                    PathBuf::from(stripped)
                } else {
                    canonical
                }
            }
            #[cfg(not(windows))]
            canonical
        }
        Err(_) => resolved,
    }
}

fn workspace_storage_name(workspace: &Path) -> String {
    let resolved = canonicalize_codegraph_path(workspace);
    let resolved_str = resolved.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(resolved_str.as_bytes());
    let full_hash = hex::encode(hasher.finalize());
    let short_hash = &full_hash[..16];
    let file_name = resolved
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let base = sanitize_base(&file_name);
    format!("{base}-{short_hash}")
}

pub fn resolve_codegraph_workspace_paths(
    workspace: &Path,
    options: &ResolveCodegraphWorkspacePathsOptions,
) -> CodegraphWorkspacePaths {
    let resolved_workspace = canonicalize_codegraph_path(workspace);
    let data_root = options
        .home_dir
        .as_ref()
        .map(|p| codegraph_data_root(p))
        .or_else(|| dirs::home_dir().map(|h| codegraph_data_root(&h)))
        .unwrap_or_else(|| codegraph_data_root(Path::new("")));
    let storage_name = workspace_storage_name(&resolved_workspace);
    CodegraphWorkspacePaths {
        data_dir: data_root.join("projects").join(storage_name),
        data_root,
        project_link: resolved_workspace.join(".codegraph"),
    }
}
