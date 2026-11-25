//! Port of senpi `packages/coding-agent/src/modes/rpc/project-file-commands.ts`.
//!
//! `read_project_file` / `write_project_file`: read and edit the session workspace's instruction
//! files over RPC, so a client with no filesystem access to the machine edits Rules the way a local
//! one does.

use crate::project_file_path::{
    PROJECT_FILE_MAX_BYTES, ProjectFilePath, ResolvedProjectFile, parse_project_file_path,
    resolve_project_file,
};
use crate::project_file_store::{Stored, etag_of, read_stored, serialize_write};
use crate::project_write_gate::{
    ProjectWriteRefusal, RPC_ERROR_PROJECT_FILE_NOT_ALLOWED, RPC_ERROR_PROJECT_FILE_TOO_LARGE,
    WriteAuthorizer, commit_guarded_write, conflict, not_trusted,
};
use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use maho_core::agent_session::AgentSession;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectFile {
    pub cwd: String,
    /// The canonical workspace-relative path.
    pub path: String,
    pub exists: bool,
    pub content: Option<String>,
    /// Opaque version of the stored bytes; compare it, never parse it.
    pub etag: Option<String>,
    pub project_trusted: bool,
}

pub type ProjectFileResult = Result<RpcProjectFile, ProjectWriteRefusal>;

struct FileState {
    data: RpcProjectFile,
    mode: Option<u32>,
    absolute: PathBuf,
    missing_folders: Vec<PathBuf>,
}

fn not_allowed(path: &str, reason: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!(
            "{path} is not a project file inside this workspace (AGENTS.md, CLAUDE.md, .omo/rules/** or .omo/omo.jsonc)."
        ),
        error_code: RPC_ERROR_PROJECT_FILE_NOT_ALLOWED.to_owned(),
        error_data: Some(json!({ "path": path, "reason": reason })),
    }
}

fn too_large(path: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!("{path} is larger than {PROJECT_FILE_MAX_BYTES} bytes."),
        error_code: RPC_ERROR_PROJECT_FILE_TOO_LARGE.to_owned(),
        error_data: Some(json!({ "limitBytes": PROJECT_FILE_MAX_BYTES })),
    }
}

fn parse_path(command: &str, path: Option<&Value>) -> Result<ProjectFilePath, ProjectWriteRefusal> {
    let Some(path_str) = path.and_then(Value::as_str) else {
        return Err(ProjectWriteRefusal {
            message: format!("{command} requires a string 'path'."),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    parse_project_file_path(path_str).ok_or_else(|| not_allowed(path_str, "not_listed"))
}

async fn read_file_state(
    session: &AgentSession,
    file: &ProjectFilePath,
) -> Result<FileState, ProjectWriteRefusal> {
    let cwd = session.cwd();
    let project_trusted = session.with_settings_manager(|s| s.is_project_trusted());
    let resolved = resolve_project_file(Path::new(&cwd), file)
        .await
        .map_err(|_| not_allowed(&file.relative, "unsafe"))?;
    let ResolvedProjectFile::Ok {
        absolute,
        exists,
        missing_folders,
    } = resolved
    else {
        return Err(not_allowed(&file.relative, "unsafe"));
    };
    let stored = if exists {
        read_stored(&absolute)
            .await
            .map_err(|_| not_allowed(&file.relative, "unsafe"))?
    } else {
        Stored::Missing
    };
    match stored {
        Stored::Refused => Err(not_allowed(&file.relative, "unsafe")),
        Stored::TooLarge => Err(too_large(&file.relative)),
        Stored::Missing => Ok(FileState {
            data: RpcProjectFile {
                cwd,
                path: file.relative.clone(),
                exists: false,
                content: None,
                etag: None,
                project_trusted,
            },
            mode: None,
            absolute,
            missing_folders,
        }),
        Stored::File { bytes, mode } => {
            let etag = etag_of(&bytes);
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok(FileState {
                data: RpcProjectFile {
                    cwd,
                    path: file.relative.clone(),
                    exists: true,
                    content: Some(content),
                    etag: Some(etag),
                    project_trusted,
                },
                mode: Some(mode),
                absolute,
                missing_folders,
            })
        }
    }
}

pub async fn read_project_file(session: &AgentSession, path: Option<&Value>) -> ProjectFileResult {
    let file = parse_path("read_project_file", path)?;
    let state = read_file_state(session, &file).await?;
    Ok(state.data)
}

async fn write_checked(
    session: &AgentSession,
    file: &ProjectFilePath,
    content: &str,
    if_etag: Option<&str>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectFileResult {
    let state = read_file_state(session, file).await?;
    if state.data.etag.as_deref() != if_etag {
        return Err(conflict(&file.relative, state.data.etag.as_deref()));
    }
    if let Some(refusal) = commit_guarded_write(
        session,
        crate::project_write_gate::GuardedWrite {
            relative: file.relative.clone(),
            absolute: state.absolute,
            content: content.to_owned(),
            expected_etag: if_etag.map(str::to_owned),
            mode: state.mode,
            missing_folders: state.missing_folders,
        },
        authorizer,
    )
    .await
    {
        return Err(refusal);
    }
    let updated = read_file_state(session, file).await?;
    Ok(updated.data)
}

pub async fn write_project_file(
    session: &AgentSession,
    path: Option<&Value>,
    content: Option<&Value>,
    if_etag: Option<&Value>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectFileResult {
    let file = parse_path("write_project_file", path)?;
    let Some(content_str) = content.and_then(Value::as_str) else {
        return Err(ProjectWriteRefusal {
            message: "write_project_file requires a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    let parsed_etag = match if_etag {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        _ => {
            return Err(ProjectWriteRefusal {
                message: "write_project_file requires a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
                error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
                error_data: None,
            });
        }
    };
    if !session.with_settings_manager(|s| s.is_project_trusted()) {
        return Err(not_trusted());
    }
    if content_str.len() > PROJECT_FILE_MAX_BYTES {
        return Err(too_large(&file.relative));
    }
    let key = Path::new(&session.cwd()).join(&file.relative);
    serialize_write(key, || {
        write_checked(session, &file, content_str, parsed_etag, authorizer)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_rejects_non_string_and_unlisted_paths() {
        let non_str = parse_path("read_project_file", Some(&json!(42))).unwrap_err();
        assert_eq!(non_str.error_code, RPC_ERROR_INVALID_ARGUMENT);
        let unlisted = parse_path("read_project_file", Some(&json!("README.md"))).unwrap_err();
        assert_eq!(unlisted.error_code, RPC_ERROR_PROJECT_FILE_NOT_ALLOWED);
        assert_eq!(
            unlisted.error_data,
            Some(json!({ "path": "README.md", "reason": "not_listed" }))
        );
        let ok = parse_path("read_project_file", Some(&json!("AGENTS.md"))).expect("valid");
        assert_eq!(ok.relative, "AGENTS.md");
    }
}
