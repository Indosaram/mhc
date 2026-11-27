//! Port of senpi `packages/coding-agent/src/modes/rpc/project-write-gate.ts`.
//!
//! The gates every folder-file write over RPC passes right before it commits: the project trust as
//! it stands at that moment, and the permission decision the same edit would get from this
//! machine's own session.

use crate::project_file_path::{PROJECT_FILE_MAX_BYTES, create_missing_folders};
use crate::project_file_store::{
    ReplaceAtomicallyError, Stored, etag_of, read_stored, replace_atomically,
};
use futures::future::BoxFuture;
use maho_core::agent_session::AgentSession;
use maho_core::trust_manager::{ProjectTrustStore, get_saved_project_trust};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const RPC_ERROR_PROJECT_FILE_CONFLICT: &str = "project_file_conflict";
pub const RPC_ERROR_PROJECT_FILE_NOT_ALLOWED: &str = "project_file_not_allowed";
pub const RPC_ERROR_PROJECT_FILE_PERMISSION_DENIED: &str = "project_file_permission_denied";
pub const RPC_ERROR_PROJECT_FILE_TOO_LARGE: &str = "project_file_too_large";
pub const RPC_ERROR_PROJECT_FILE_WRITE_FAILED: &str = "project_file_write_failed";
pub const RPC_ERROR_PROJECT_NOT_TRUSTED: &str = "project_not_trusted";

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectWriteRefusal {
    pub message: String,
    pub error_code: String,
    pub error_data: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct GuardedWrite {
    pub relative: String,
    pub absolute: PathBuf,
    pub content: String,
    /// The stored version the write replaces (`None`: none); it must still be current when the write commits.
    pub expected_etag: Option<String>,
    pub mode: Option<u32>,
    pub missing_folders: Vec<PathBuf>,
}

/// Fence returned by a permission authorizer, re-checked immediately before committing the write.
pub type WriteFence = Box<dyn Fn() -> bool + Send + Sync>;

/// Injected permission-system write authorizer (`maho-rpc` cannot depend on `maho-ext-permission-system`).
pub type WriteAuthorizer =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<WriteFence, String>> + Send + Sync>;

pub fn not_allowed(relative: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!(
            "{relative} is not a file this folder lets an RPC client edit (outside the folder, or through a link)."
        ),
        error_code: RPC_ERROR_PROJECT_FILE_NOT_ALLOWED.to_owned(),
        error_data: Some(json!({ "path": relative })),
    }
}

pub fn too_large() -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!("The file is larger than {PROJECT_FILE_MAX_BYTES} bytes."),
        error_code: RPC_ERROR_PROJECT_FILE_TOO_LARGE.to_owned(),
        error_data: Some(json!({ "limitBytes": PROJECT_FILE_MAX_BYTES })),
    }
}

pub fn conflict(relative: &str, current_etag: Option<&str>) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!(
            "{relative} changed since that version was read; read it again before writing."
        ),
        error_code: RPC_ERROR_PROJECT_FILE_CONFLICT.to_owned(),
        error_data: Some(json!({
            "currentEtag": current_etag,
            "exists": current_etag.is_some(),
        })),
    }
}

pub fn not_trusted() -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: "Project is not trusted; refusing to write project files. Trust it with set_project_trust first.".to_owned(),
        error_code: RPC_ERROR_PROJECT_NOT_TRUSTED.to_owned(),
        error_data: None,
    }
}

pub fn permission_denied(reason: impl Into<String>) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: reason.into(),
        error_code: RPC_ERROR_PROJECT_FILE_PERMISSION_DENIED.to_owned(),
        error_data: None,
    }
}

pub async fn project_trusted_for_write(session: &AgentSession) -> bool {
    let (trusted, locked, from_full_access) = session.with_settings_manager(|settings| {
        (
            settings.is_project_trusted(),
            settings.is_project_trust_locked(),
            settings.is_trust_from_full_access(),
        )
    });
    if !trusted {
        return false;
    }
    if locked || from_full_access {
        return true;
    }
    let store = ProjectTrustStore::new(&session.agent_dir());
    !matches!(
        get_saved_project_trust(&store, &session.cwd()).await,
        Ok(Some(false))
    )
}

async fn authorize_write(
    absolute: &Path,
    content: &str,
    authorizer: Option<&WriteAuthorizer>,
) -> Result<WriteFence, ProjectWriteRefusal> {
    let Some(authorizer) = authorizer else {
        return Ok(Box::new(|| true));
    };
    authorizer(absolute.display().to_string(), content.to_owned())
        .await
        .map_err(permission_denied)
}

/// Commits one checked write: the permission decision, then trust and the expected version again
/// (an ask can take a while), the permission fence, and the atomic replace.
pub async fn commit_guarded_write(
    session: &AgentSession,
    write: GuardedWrite,
    authorizer: Option<&WriteAuthorizer>,
) -> Option<ProjectWriteRefusal> {
    let fence = match authorize_write(&write.absolute, &write.content, authorizer).await {
        Ok(fence) => fence,
        Err(refusal) => return Some(refusal),
    };
    if !project_trusted_for_write(session).await {
        return Some(not_trusted());
    }
    let stored = if !write.missing_folders.is_empty() {
        Stored::Missing
    } else {
        match read_stored(&write.absolute).await {
            Ok(stored) => stored,
            Err(err) => {
                return Some(ProjectWriteRefusal {
                    message: format!("Writing {} failed: {err}", write.relative),
                    error_code: RPC_ERROR_PROJECT_FILE_WRITE_FAILED.to_owned(),
                    error_data: None,
                });
            }
        }
    };
    match stored {
        Stored::Refused => return Some(not_allowed(&write.relative)),
        Stored::TooLarge => return Some(too_large()),
        Stored::Missing => {
            if write.expected_etag.is_some() {
                return Some(conflict(&write.relative, None));
            }
        }
        Stored::File { bytes, .. } => {
            let current_etag = etag_of(&bytes);
            if write.expected_etag.as_deref() != Some(current_etag.as_str()) {
                return Some(conflict(&write.relative, Some(&current_etag)));
            }
        }
    }
    if !fence() {
        return Some(permission_denied(
            "The permission decision for this write changed before it was committed.",
        ));
    }
    match create_missing_folders(&write.missing_folders).await {
        Ok(true) => {}
        Ok(false) => return Some(not_allowed(&write.relative)),
        Err(err) => {
            return Some(ProjectWriteRefusal {
                message: format!("Writing {} failed: {err}", write.relative),
                error_code: RPC_ERROR_PROJECT_FILE_WRITE_FAILED.to_owned(),
                error_data: None,
            });
        }
    }
    match replace_atomically(&write.absolute, &write.content, write.mode).await {
        Ok(()) => None,
        Err(ReplaceAtomicallyError::ParentMoved(_)) => Some(not_allowed(&write.relative)),
        Err(ReplaceAtomicallyError::Io(err)) => Some(ProjectWriteRefusal {
            message: format!("Writing {} failed: {err}", write.relative),
            error_code: RPC_ERROR_PROJECT_FILE_WRITE_FAILED.to_owned(),
            error_data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_builders_match_expected_codes_and_data() {
        let c = conflict("AGENTS.md", Some("sha256-abc"));
        assert_eq!(c.error_code, RPC_ERROR_PROJECT_FILE_CONFLICT);
        assert_eq!(
            c.error_data,
            Some(json!({ "currentEtag": "sha256-abc", "exists": true }))
        );
        let c_missing = conflict("AGENTS.md", None);
        assert_eq!(
            c_missing.error_data,
            Some(json!({ "currentEtag": null, "exists": false }))
        );
        assert_eq!(not_trusted().error_code, RPC_ERROR_PROJECT_NOT_TRUSTED);
        assert_eq!(too_large().error_code, RPC_ERROR_PROJECT_FILE_TOO_LARGE);
        assert_eq!(
            not_allowed("x").error_code,
            RPC_ERROR_PROJECT_FILE_NOT_ALLOWED
        );
        assert_eq!(
            permission_denied("denied").error_code,
            RPC_ERROR_PROJECT_FILE_PERMISSION_DENIED
        );
    }
}
