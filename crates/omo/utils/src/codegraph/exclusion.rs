use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths::canonicalize_codegraph_path;
use crate::runtime::node_platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodegraphProjectExclusionReason {
    #[serde(rename = "custom-root")]
    CustomRoot,
    #[serde(rename = "omo-state")]
    OmoState,
    #[serde(rename = "tmp-root")]
    TmpRoot,
}

impl CodegraphProjectExclusionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CustomRoot => "custom-root",
            Self::OmoState => "omo-state",
            Self::TmpRoot => "tmp-root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphProjectExclusionDecision {
    pub excluded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<CodegraphProjectExclusionReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodegraphProjectExclusionOptions {
    pub excluded_roots: Option<Vec<String>>,
    pub home_dir: Option<PathBuf>,
    pub platform: Option<String>,
    pub tmpdir: Option<PathBuf>,
}

const POSIX_DEFAULT_EXCLUDED_ROOTS: &[&str] = &["/tmp", "/private/tmp"];

fn expand_home(path: &str, home_dir: &Path) -> PathBuf {
    if path == "~" {
        home_dir.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        home_dir.join(rest)
    } else {
        PathBuf::from(path)
    }
}

fn resolve_configured_root(path: &str, home_dir: &Path) -> PathBuf {
    let expanded = expand_home(path, home_dir);
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        home_dir.join(expanded)
    };
    canonicalize_codegraph_path(&resolved)
}

fn normalize_for_comparison(path_str: &str, platform: &str) -> String {
    let normalized = path_str.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    if platform == "win32" {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

fn path_is_within(path: &Path, root: &Path, platform: &str) -> bool {
    let candidate = normalize_for_comparison(&path.to_string_lossy(), platform);
    let normalized_root = normalize_for_comparison(&root.to_string_lossy(), platform);
    candidate == normalized_root || candidate.starts_with(&format!("{normalized_root}/"))
}

fn has_omo_path_segment(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".omo")
}

fn default_excluded_roots(platform: &str, tmpdir: &Path) -> Vec<PathBuf> {
    if platform == "win32" {
        vec![tmpdir.to_path_buf()]
    } else {
        let mut roots = Vec::with_capacity(POSIX_DEFAULT_EXCLUDED_ROOTS.len() + 1);
        for root in POSIX_DEFAULT_EXCLUDED_ROOTS {
            roots.push(PathBuf::from(root));
        }
        roots.push(tmpdir.to_path_buf());
        roots
    }
}

pub fn should_exclude_codegraph_project(
    workspace: &Path,
    options: &CodegraphProjectExclusionOptions,
) -> CodegraphProjectExclusionDecision {
    let platform: &str = match &options.platform {
        Some(p) => p.as_str(),
        None => node_platform(),
    };
    let home_dir = options
        .home_dir
        .clone()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from(""));
    let tmpdir = options.tmpdir.clone().unwrap_or_else(std::env::temp_dir);
    let resolved_workspace = canonicalize_codegraph_path(workspace);

    if has_omo_path_segment(&resolved_workspace) {
        return CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some(".omo".to_string()),
            reason: Some(CodegraphProjectExclusionReason::OmoState),
        };
    }

    for root in default_excluded_roots(platform, &tmpdir) {
        let resolved_root = canonicalize_codegraph_path(&root);
        if path_is_within(&resolved_workspace, &resolved_root, platform) {
            return CodegraphProjectExclusionDecision {
                excluded: true,
                matched_root: Some(root.to_string_lossy().into_owned()),
                reason: Some(CodegraphProjectExclusionReason::TmpRoot),
            };
        }
    }

    if let Some(excluded_roots) = &options.excluded_roots {
        for root in excluded_roots {
            let trimmed = root.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resolved_root = resolve_configured_root(trimmed, &home_dir);
            if path_is_within(&resolved_workspace, &resolved_root, platform) {
                return CodegraphProjectExclusionDecision {
                    excluded: true,
                    matched_root: Some(root.clone()),
                    reason: Some(CodegraphProjectExclusionReason::CustomRoot),
                };
            }
        }
    }

    CodegraphProjectExclusionDecision {
        excluded: false,
        matched_root: None,
        reason: None,
    }
}
