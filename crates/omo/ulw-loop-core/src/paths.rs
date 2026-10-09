//! Path resolution and normalization for ulw-loop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::constants::{ULW_LOOP_BRIEF, ULW_LOOP_DIR, ULW_LOOP_GOALS, ULW_LOOP_LEDGER, ULW_LOOP_STATE_LOCK};
use crate::runtime::UlwLoopError;

#[derive(Clone, Debug, Default)]
pub struct UlwLoopScope {
    pub session_id: Option<String>,
}

impl UlwLoopScope {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self { session_id: Some(session_id.into()) }
    }
}

pub fn normalize_ulw_loop_session_id(session_id: Option<&str>) -> Option<String> {
    let trimmed = session_id?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let segments: Vec<&str> = trimmed
        .split(|c| c == '/' || c == '\\')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();

    let candidate_str = if !segments.is_empty() {
        segments.join("-")
    } else {
        trimmed.to_string()
    };

    let mut out = String::new();
    let mut last_dash = false;
    for c in candidate_str.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            if c == '-' {
                if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            } else {
                out.push(c);
                last_dash = false;
            }
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }

    let trimmed_leading = out.trim_start_matches('.');
    let cleaned = trimmed_leading.trim_matches(|c| c == '.' || c == '-');
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

pub fn resolve_ulw_loop_session_id_from_env(env: &BTreeMap<String, String>) -> Option<String> {
    for key in &["OMO_ULW_LOOP_SESSION_ID", "CODEX_SESSION_ID", "CODEX_THREAD_ID", "PI_SESSION_ID"] {
        if let Some(val) = env.get(*key) {
            if let Some(norm) = normalize_ulw_loop_session_id(Some(val)) {
                return Some(norm);
            }
        }
    }
    None
}

pub fn ulw_loop_relative_dir(scope: Option<&UlwLoopScope>) -> String {
    let session_id = scope.and_then(|s| normalize_ulw_loop_session_id(s.session_id.as_deref()));
    match session_id {
        Some(id) => format!("{ULW_LOOP_DIR}/{id}"),
        None => ULW_LOOP_DIR.to_string(),
    }
}

pub fn ulw_loop_dir(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> PathBuf {
    repo_root.as_ref().join(ulw_loop_relative_dir(scope))
}

pub fn ulw_loop_brief_relative_path(scope: Option<&UlwLoopScope>) -> String {
    format!("{}/{}", ulw_loop_relative_dir(scope), ULW_LOOP_BRIEF)
}

pub fn ulw_loop_goals_relative_path(scope: Option<&UlwLoopScope>) -> String {
    format!("{}/{}", ulw_loop_relative_dir(scope), ULW_LOOP_GOALS)
}

pub fn ulw_loop_ledger_relative_path(scope: Option<&UlwLoopScope>) -> String {
    format!("{}/{}", ulw_loop_relative_dir(scope), ULW_LOOP_LEDGER)
}

pub fn ulw_loop_brief_path(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> PathBuf {
    ulw_loop_dir(repo_root, scope).join(ULW_LOOP_BRIEF)
}

pub fn ulw_loop_goals_path(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> PathBuf {
    ulw_loop_dir(repo_root, scope).join(ULW_LOOP_GOALS)
}

pub fn ulw_loop_ledger_path(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> PathBuf {
    ulw_loop_dir(repo_root, scope).join(ULW_LOOP_LEDGER)
}

pub fn ulw_loop_state_lock_path(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> PathBuf {
    ulw_loop_dir(repo_root, scope).join(ULW_LOOP_STATE_LOCK)
}

pub fn repo_relative(absolute_path: &str, repo_root: &str) -> String {
    let slash_prefix = format!("{repo_root}/");
    let backslash_prefix = format!("{repo_root}\\");
    if let Some(rest) = absolute_path.strip_prefix(&slash_prefix) {
        return rest.replace('\\', "/");
    }
    if let Some(rest) = absolute_path.strip_prefix(&backslash_prefix) {
        return rest.replace('\\', "/");
    }
    absolute_path.replace('\\', "/")
}

pub fn ulw_loop_evidence_root(scope: Option<&UlwLoopScope>) -> String {
    let session_id = scope.and_then(|s| normalize_ulw_loop_session_id(s.session_id.as_deref()));
    match session_id {
        Some(id) => format!(".omo/evidence/ulw/{id}"),
        None => ".omo/evidence".to_string(),
    }
}

pub fn ulw_loop_attempt_evidence_dir(
    goal_id: &str,
    attempt: u32,
    scope: Option<&UlwLoopScope>,
) -> Result<String, UlwLoopError> {
    let session_id = scope.and_then(|s| normalize_ulw_loop_session_id(s.session_id.as_deref()));
    match session_id {
        Some(id) => Ok(format!(".omo/evidence/ulw/{id}/{goal_id}/a{attempt}")),
        None => {
            let mut details = BTreeMap::new();
            details.insert("goalId".into(), serde_json::json!(goal_id));
            details.insert("attempt".into(), serde_json::json!(attempt));
            Err(UlwLoopError::with_details(
                format!("Evidence for {goal_id} attempt {attempt} needs a session scope; pass --session-id <id> so the attempt directory lives under .omo/evidence/ulw/<id>/."),
                "ULW_LOOP_SESSION_SCOPE_REQUIRED",
                details,
            ))
        }
    }
}

pub fn is_within_attempt_dir(absolute_path: &str, attempt_root: &str) -> bool {
    let sep = if attempt_root.contains('\\') { '\\' } else { '/' };
    let norm_path = if sep == '/' {
        absolute_path.replace('\\', "/")
    } else {
        absolute_path.replace('/', "\\")
    };
    let norm_root = if sep == '/' {
        attempt_root.replace('\\', "/")
    } else {
        attempt_root.replace('/', "\\")
    };

    if norm_path == norm_root {
        return true;
    }

    let prefix = format!("{norm_root}{sep}");
    norm_path.starts_with(&prefix)
}
