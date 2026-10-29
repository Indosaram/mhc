use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths::{
    ResolveCodegraphWorkspacePathsOptions, canonicalize_codegraph_path,
    resolve_codegraph_workspace_paths,
};
use super::store::write_codegraph_source_metadata;
use crate::runtime::node_platform;

pub type SymlinkFn<'a> = &'a dyn Fn(&Path, &Path, &str) -> std::io::Result<()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodegraphWorkspaceMode {
    #[serde(rename = "global-linked")]
    GlobalLinked,
    #[serde(rename = "in-place-fallback")]
    InPlaceFallback,
    #[serde(rename = "in-project")]
    InProject,
}

impl CodegraphWorkspaceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GlobalLinked => "global-linked",
            Self::InPlaceFallback => "in-place-fallback",
            Self::InProject => "in-project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphWorkspacePreparation {
    pub data_dir: PathBuf,
    pub data_root: PathBuf,
    pub linked: bool,
    pub mode: CodegraphWorkspaceMode,
    pub project_link: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Default)]
pub struct PrepareCodegraphWorkspaceOptions<'a> {
    pub home_dir: Option<PathBuf>,
    pub platform: Option<String>,
    pub same_filesystem: Option<bool>,
    pub symlink: Option<SymlinkFn<'a>>,
}

fn ensure_in_place_fallback(project_link: &Path) -> std::io::Result<()> {
    if !project_link.exists() {
        std::fs::create_dir_all(project_link)?;
    }
    Ok(())
}

fn is_same_filesystem(workspace: &Path, data_root: &Path, override_val: Option<bool>) -> bool {
    if let Some(val) = override_val {
        return val;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let w_dev = std::fs::metadata(workspace).map(|m| m.dev()).ok();
        let d_dev = std::fs::metadata(data_root).map(|m| m.dev()).ok();
        matches!((w_dev, d_dev), (Some(a), Some(b)) if a == b)
    }
    #[cfg(not(unix))]
    {
        let _ = workspace;
        let _ = data_root;
        true
    }
}

fn create_symlink(target: &Path, link: &Path, platform: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let _ = platform;
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let _ = platform;
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        let _ = target;
        let _ = link;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks not supported on this platform",
        ))
    }
}

pub fn prepare_codegraph_workspace(
    workspace: &Path,
    options: &PrepareCodegraphWorkspaceOptions<'_>,
) -> CodegraphWorkspacePreparation {
    let resolved_workspace = canonicalize_codegraph_path(workspace);
    let paths = resolve_codegraph_workspace_paths(
        &resolved_workspace,
        &ResolveCodegraphWorkspacePathsOptions {
            home_dir: options.home_dir.clone(),
        },
    );

    let fallback_result = |reason: String| CodegraphWorkspacePreparation {
        data_dir: paths.project_link.clone(),
        data_root: paths.data_root.clone(),
        linked: false,
        mode: CodegraphWorkspaceMode::InPlaceFallback,
        project_link: paths.project_link.clone(),
        reason: Some(reason),
    };

    if let Err(e) = std::fs::create_dir_all(&paths.data_dir) {
        let _ = ensure_in_place_fallback(&paths.project_link);
        return fallback_result(e.to_string());
    }

    if let Err(e) = write_codegraph_source_metadata(&paths.data_dir, &resolved_workspace) {
        let _ = ensure_in_place_fallback(&paths.project_link);
        return fallback_result(e.to_string());
    }

    if paths.project_link.symlink_metadata().is_ok() {
        let is_symlink = paths
            .project_link
            .symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(false);
        if !is_symlink {
            return CodegraphWorkspacePreparation {
                data_dir: paths.project_link.clone(),
                data_root: paths.data_root,
                linked: false,
                mode: CodegraphWorkspaceMode::InProject,
                project_link: paths.project_link,
                reason: None,
            };
        }

        let link_real = canonicalize_codegraph_path(&paths.project_link);
        let data_real = canonicalize_codegraph_path(&paths.data_dir);
        if link_real == data_real {
            return CodegraphWorkspacePreparation {
                data_dir: paths.data_dir,
                data_root: paths.data_root,
                linked: true,
                mode: CodegraphWorkspaceMode::GlobalLinked,
                project_link: paths.project_link,
                reason: None,
            };
        }

        let _ = ensure_in_place_fallback(&paths.project_link);
        return fallback_result("existing .codegraph symlink points outside OMO store".to_string());
    }

    if !is_same_filesystem(
        &resolved_workspace,
        &paths.data_root,
        options.same_filesystem,
    ) {
        let _ = ensure_in_place_fallback(&paths.project_link);
        return fallback_result("workspace and OMO store are on different filesystems".to_string());
    }

    let platform: &str = match &options.platform {
        Some(p) => p.as_str(),
        None => node_platform(),
    };

    let symlink_res = match options.symlink {
        Some(f) => f(&paths.data_dir, &paths.project_link, platform),
        None => create_symlink(&paths.data_dir, &paths.project_link, platform),
    };

    match symlink_res {
        Ok(()) => CodegraphWorkspacePreparation {
            data_dir: paths.data_dir,
            data_root: paths.data_root,
            linked: true,
            mode: CodegraphWorkspaceMode::GlobalLinked,
            project_link: paths.project_link,
            reason: None,
        },
        Err(e) => {
            let reason = e.to_string();
            if let Err(fallback_err) = ensure_in_place_fallback(&paths.project_link) {
                fallback_result(format!("{reason}; fallback failed: {fallback_err}"))
            } else {
                fallback_result(reason)
            }
        }
    }
}

fn run_git_cmd(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git_config_get(config_file: &Path, key: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "--file"])
        .arg(config_file)
        .args(["--get", key])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_marker_owns_git_dir(git_marker_path: &Path, workspace: &Path, git_dir: &Path) -> bool {
    let Ok(marker_stat) = std::fs::symlink_metadata(git_marker_path) else {
        return false;
    };
    if marker_stat.is_symlink() {
        return false;
    }
    let resolved_git_dir = canonicalize_codegraph_path(git_dir);
    if marker_stat.is_dir() {
        return canonicalize_codegraph_path(git_marker_path) == resolved_git_dir;
    }
    if !marker_stat.is_file() {
        return false;
    }
    let Ok(marker_content) = std::fs::read_to_string(git_marker_path) else {
        return false;
    };
    let trimmed_marker = marker_content.trim();
    let Some(gitdir_rel) = trimmed_marker.strip_prefix("gitdir:") else {
        return false;
    };
    let gitdir_target = gitdir_rel.trim();
    let target_path = workspace.join(gitdir_target);
    if canonicalize_codegraph_path(&target_path) != resolved_git_dir {
        return false;
    }

    let backlink_path = resolved_git_dir.join("gitdir");
    if backlink_path.exists()
        && let Ok(backlink) = std::fs::read_to_string(&backlink_path)
    {
        let trimmed = backlink.trim();
        if !trimmed.is_empty() {
            let resolved_backlink = canonicalize_codegraph_path(&resolved_git_dir.join(trimmed));
            let resolved_marker = canonicalize_codegraph_path(git_marker_path);
            if resolved_backlink == resolved_marker {
                return true;
            }
        }
    }

    if let Some(core_worktree) =
        run_git_config_get(&resolved_git_dir.join("config"), "core.worktree")
        && !core_worktree.is_empty()
    {
        let resolved_worktree = canonicalize_codegraph_path(&resolved_git_dir.join(&core_worktree));
        let resolved_workspace = canonicalize_codegraph_path(workspace);
        return resolved_worktree == resolved_workspace;
    }

    false
}

pub fn ensure_codegraph_gitignored(workspace: &Path) -> bool {
    let git_marker_path = workspace.join(".git");
    if !git_marker_path.exists() {
        return false;
    }

    let is_worktree = run_git_cmd(workspace, &["rev-parse", "--is-inside-work-tree"]);
    if is_worktree.as_deref() != Some("true") {
        return false;
    }

    let Some(git_top_level) = run_git_cmd(workspace, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    if git_top_level.is_empty() {
        return false;
    }
    let real_top_level = canonicalize_codegraph_path(Path::new(&git_top_level));
    let real_workspace = canonicalize_codegraph_path(workspace);
    if real_top_level != real_workspace {
        return false;
    }

    let Some(git_dir_str) = run_git_cmd(workspace, &["rev-parse", "--absolute-git-dir"]) else {
        return false;
    };
    if git_dir_str.is_empty()
        || !git_marker_owns_git_dir(&git_marker_path, workspace, Path::new(&git_dir_str))
    {
        return false;
    }

    let Some(git_exclude_path) =
        run_git_cmd(workspace, &["rev-parse", "--git-path", "info/exclude"])
    else {
        return false;
    };
    if git_exclude_path.is_empty() {
        return false;
    }

    let exclude_path = workspace.join(&git_exclude_path);
    if let Some(parent) = exclude_path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line == ".codegraph") {
        return true;
    }
    let prefix = if existing.ends_with('\n') || existing.is_empty() {
        ""
    } else {
        "\n"
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
    else {
        return false;
    };
    file.write_all(format!("{prefix}.codegraph\n").as_bytes())
        .is_ok()
}
