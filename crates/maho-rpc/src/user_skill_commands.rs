//! Port of senpi `packages/coding-agent/src/modes/rpc/user-skill-commands.ts`.
//!
//! `read_user_skill_file` / `write_user_skill_file`: read and edit the file of a user-global skill
//! the engine lists for the session (one under the agent dir's skills root or `~/.agents/skills`),
//! by name, on this machine or from another device of the account through the same verb.

use crate::project_file_path::{
    PROJECT_FILE_MAX_BYTES, ProjectFilePath, ResolvedProjectFile, resolve_project_file,
};
use crate::project_file_store::{
    ReplaceAtomicallyError, Stored, etag_of, read_stored, replace_atomically, serialize_write,
};
use crate::project_skill_commands::folder_relative;
use crate::project_write_gate::{
    ProjectWriteRefusal, RPC_ERROR_PROJECT_FILE_WRITE_FAILED, conflict, too_large,
};
use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use crate::user_skill_approval::{
    LocalGrantSelector, RpcRemoteOrigin, UserSkillAuditEntry, UserSkillDecision,
    append_user_skill_audit, ask_this_device, refused_by_device, sha256_hex,
    user_skill_prompt_title,
};
use maho_core::agent_session::AgentSession;
use maho_core::config::home_dir;
use maho_core::skills::Skill;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const RPC_ERROR_USER_SKILL_NOT_LISTED: &str = "user_skill_not_listed";
pub const RPC_ERROR_USER_SKILL_FILE_NOT_ALLOWED: &str = "user_skill_file_not_allowed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserSkillFile {
    pub skill: String,
    pub path: String,
    pub exists: bool,
    pub content: Option<String>,
    pub etag: Option<String>,
    /// Set when the write's audit line could not be written; the write's own outcome is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_error: Option<String>,
}

pub type UserSkillFileResult = Result<RpcUserSkillFile, ProjectWriteRefusal>;

#[derive(Debug, Clone)]
pub struct LocatedUserSkill {
    pub root: PathBuf,
    pub file: ProjectFilePath,
}

struct SkillState {
    data: RpcUserSkillFile,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
    absolute: PathBuf,
}

fn not_listed(skill: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!("No user skill named {skill:?} is listed for this session."),
        error_code: RPC_ERROR_USER_SKILL_NOT_LISTED.to_owned(),
        error_data: Some(json!({ "skill": skill })),
    }
}

fn not_allowed(skill: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!(
            "The file of {skill:?} is not inside a user skills folder (or is reached through a link)."
        ),
        error_code: RPC_ERROR_USER_SKILL_FILE_NOT_ALLOWED.to_owned(),
        error_data: Some(json!({ "skill": skill })),
    }
}

pub fn user_skill_roots(agent_dir: &str) -> Vec<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(home_dir);
    vec![
        PathBuf::from(agent_dir).join("skills"),
        PathBuf::from(home).join(".agents").join("skills"),
    ]
}

async fn real_or_none(path: &Path) -> Option<PathBuf> {
    tokio::fs::canonicalize(path)
        .await
        .ok()
        .map(maho_core::paths::strip_verbatim_prefix)
}

pub async fn locate_user_skill_from_list(
    skills: &[Skill],
    agent_dir: &str,
    skill: &str,
) -> Result<LocatedUserSkill, ProjectWriteRefusal> {
    let Some(listed) = skills.iter().find(|candidate| candidate.name == skill) else {
        return Err(not_listed(skill));
    };
    let listed_path = Path::new(&listed.file_path);
    for root in user_skill_roots(agent_dir) {
        let Some(real) = real_or_none(&root).await else {
            continue;
        };
        let mut bases = HashSet::new();
        bases.insert(root);
        bases.insert(real.clone());
        for base in bases {
            if let Some(file) = folder_relative(&base, listed_path) {
                return Ok(LocatedUserSkill { root: real, file });
            }
        }
    }
    Err(not_allowed(skill))
}

async fn locate_user_skill(
    session: &AgentSession,
    skill: &str,
) -> Result<LocatedUserSkill, ProjectWriteRefusal> {
    let skills = session
        .loaded_skills()
        .map(|loaded| loaded.skills)
        .unwrap_or_default();
    locate_user_skill_from_list(&skills, &session.agent_dir(), skill).await
}

async fn read_skill_state(
    skill: &str,
    located: &LocatedUserSkill,
) -> Result<SkillState, ProjectWriteRefusal> {
    let resolved = resolve_project_file(&located.root, &located.file)
        .await
        .map_err(|_| not_allowed(skill))?;
    let ResolvedProjectFile::Ok {
        absolute,
        exists,
        missing_folders,
    } = resolved
    else {
        return Err(not_allowed(skill));
    };
    if !missing_folders.is_empty() {
        return Err(not_allowed(skill));
    }
    let stored = if exists {
        read_stored(&absolute)
            .await
            .map_err(|_| not_allowed(skill))?
    } else {
        Stored::Missing
    };
    match stored {
        Stored::Refused => Err(not_allowed(skill)),
        Stored::TooLarge => Err(too_large()),
        Stored::Missing => Ok(SkillState {
            data: RpcUserSkillFile {
                skill: skill.to_owned(),
                path: absolute.display().to_string(),
                exists: false,
                content: None,
                etag: None,
                audit_error: None,
            },
            bytes: None,
            mode: None,
            absolute,
        }),
        Stored::File { bytes, mode } => {
            let etag = etag_of(&bytes);
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok(SkillState {
                data: RpcUserSkillFile {
                    skill: skill.to_owned(),
                    path: absolute.display().to_string(),
                    exists: true,
                    content: Some(content),
                    etag: Some(etag),
                    audit_error: None,
                },
                bytes: Some(bytes),
                mode: Some(mode),
                absolute,
            })
        }
    }
}

pub async fn read_user_skill_file(
    session: &AgentSession,
    skill: Option<&Value>,
) -> UserSkillFileResult {
    let Some(skill_name) = skill.and_then(Value::as_str) else {
        return Err(ProjectWriteRefusal {
            message: "read_user_skill_file requires a string 'skill'.".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    let located = locate_user_skill(session, skill_name).await?;
    let state = read_skill_state(skill_name, &located).await?;
    Ok(state.data)
}

async fn commit(
    skill: &str,
    located: &LocatedUserSkill,
    content: &str,
    expected_etag: Option<&str>,
) -> UserSkillFileResult {
    let state = read_skill_state(skill, located).await?;
    if state.data.etag.as_deref() != expected_etag {
        return Err(conflict(&state.data.path, state.data.etag.as_deref()));
    }
    match replace_atomically(&state.absolute, content, state.mode).await {
        Ok(()) => {}
        Err(ReplaceAtomicallyError::ParentMoved(_)) => return Err(not_allowed(skill)),
        Err(ReplaceAtomicallyError::Io(err)) => {
            return Err(ProjectWriteRefusal {
                message: format!("Writing {} failed: {err}", state.data.path),
                error_code: RPC_ERROR_PROJECT_FILE_WRITE_FAILED.to_owned(),
                error_data: None,
            });
        }
    }
    let after = read_skill_state(skill, located).await?;
    Ok(after.data)
}

struct Attempt {
    result: UserSkillFileResult,
    decision: UserSkillDecision,
    path: Option<String>,
    before: Option<String>,
    after: Option<String>,
}

fn refused_before(refusal: ProjectWriteRefusal, state: Option<&SkillState>) -> Attempt {
    let hash = sha256_hex(state.and_then(|s| s.bytes.as_deref()));
    Attempt {
        result: Err(refusal),
        decision: UserSkillDecision::Refused,
        path: state.map(|s| s.data.path.clone()),
        before: hash.clone(),
        after: hash,
    }
}

async fn write_checked(
    session: &AgentSession,
    skill: &str,
    located: &LocatedUserSkill,
    content: &str,
    if_etag: Option<&str>,
    origin: Option<&RpcRemoteOrigin>,
    selector: Option<&LocalGrantSelector>,
) -> Attempt {
    let state = match read_skill_state(skill, located).await {
        Ok(state) => state,
        Err(refusal) => return refused_before(refusal, None),
    };
    if content.len() > PROJECT_FILE_MAX_BYTES {
        return refused_before(too_large(), Some(&state));
    }
    if state.data.etag.as_deref() != if_etag {
        return refused_before(
            conflict(&state.data.path, state.data.etag.as_deref()),
            Some(&state),
        );
    }
    let path = state.data.path.clone();
    let before = sha256_hex(state.bytes.as_deref());
    let Some(origin) = origin else {
        return Attempt {
            result: commit(skill, located, content, if_etag).await,
            decision: UserSkillDecision::Approved,
            path: Some(path),
            before,
            after: None,
        };
    };
    let title = user_skill_prompt_title(
        origin,
        &state.data.path,
        state.data.content.as_deref().unwrap_or(""),
        content,
    );
    let decision = ask_this_device(session, title, selector).await;
    let result = if decision == UserSkillDecision::Approved {
        commit(skill, located, content, if_etag).await
    } else {
        Err(refused_by_device(decision, origin))
    };
    let after = match read_skill_state(skill, located).await {
        Ok(now) => sha256_hex(now.bytes.as_deref()),
        Err(_) => None,
    };
    Attempt {
        result,
        decision,
        path: Some(path),
        before,
        after,
    }
}

async fn attempt_write(
    session: &AgentSession,
    skill: Option<&Value>,
    content: Option<&Value>,
    if_etag: Option<&Value>,
    origin: Option<&RpcRemoteOrigin>,
    selector: Option<&LocalGrantSelector>,
) -> Attempt {
    let (Some(skill_name), Some(content_str)) = (
        skill.and_then(Value::as_str),
        content.and_then(Value::as_str),
    ) else {
        return refused_before(
            ProjectWriteRefusal {
                message: "write_user_skill_file requires a string 'skill', a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
                error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
                error_data: None,
            },
            None,
        );
    };
    let parsed_etag = match if_etag {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        _ => {
            return refused_before(
                ProjectWriteRefusal {
                    message: "write_user_skill_file requires a string 'skill', a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
                    error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
                    error_data: None,
                },
                None,
            );
        }
    };
    let located = match locate_user_skill(session, skill_name).await {
        Ok(located) => located,
        Err(refusal) => return refused_before(refusal, None),
    };
    let key = located.root.join(&located.file.relative);
    serialize_write(key, || {
        write_checked(
            session,
            skill_name,
            &located,
            content_str,
            parsed_etag,
            origin,
            selector,
        )
    })
    .await
}

fn with_audit_error(result: UserSkillFileResult, audit_error: String) -> UserSkillFileResult {
    match result {
        Ok(mut data) => {
            data.audit_error = Some(audit_error);
            Ok(data)
        }
        Err(mut refusal) => {
            let mut obj = refusal
                .error_data
                .take()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            obj.insert("auditError".to_owned(), Value::String(audit_error));
            refusal.error_data = Some(Value::Object(obj));
            Err(refusal)
        }
    }
}

pub async fn write_user_skill_file(
    session: &AgentSession,
    skill: Option<&Value>,
    content: Option<&Value>,
    if_etag: Option<&Value>,
    origin: Option<&RpcRemoteOrigin>,
    selector: Option<&LocalGrantSelector>,
) -> UserSkillFileResult {
    let attempt = attempt_write(session, skill, content, if_etag, origin, selector).await;
    let Some(origin) = origin else {
        return attempt.result;
    };
    let reason = attempt.result.as_ref().err().map(|r| r.error_code.clone());
    let audit_res = append_user_skill_audit(
        Path::new(&session.agent_dir()),
        &UserSkillAuditEntry {
            origin: origin.clone(),
            path: attempt.path,
            before: attempt.before,
            after: attempt.after,
            decision: attempt.decision,
            reason,
        },
    )
    .await;
    match audit_res {
        Ok(()) => attempt.result,
        Err(err) => with_audit_error(attempt.result, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn locate_user_skill_enforces_user_roots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let user_skill = agent_dir.join("skills/my-skill/SKILL.md");
        let outside = dir.path().join("project/.omo/skills/proj-skill/SKILL.md");
        std::fs::create_dir_all(user_skill.parent().expect("parent")).expect("mkdir");
        std::fs::create_dir_all(outside.parent().expect("parent")).expect("mkdir");
        std::fs::write(&user_skill, "body").expect("write");
        std::fs::write(&outside, "proj").expect("write");

        let skills = vec![
            Skill {
                name: "my-skill".into(),
                file_path: user_skill.display().to_string(),
                ..Default::default()
            },
            Skill {
                name: "proj-skill".into(),
                file_path: outside.display().to_string(),
                ..Default::default()
            },
        ];

        let located =
            locate_user_skill_from_list(&skills, &agent_dir.display().to_string(), "my-skill")
                .await
                .expect("located");
        assert_eq!(located.file.relative, "my-skill/SKILL.md");

        let err =
            locate_user_skill_from_list(&skills, &agent_dir.display().to_string(), "proj-skill")
                .await
                .unwrap_err();
        assert_eq!(err.error_code, RPC_ERROR_USER_SKILL_FILE_NOT_ALLOWED);

        let missing =
            locate_user_skill_from_list(&skills, &agent_dir.display().to_string(), "missing")
                .await
                .unwrap_err();
        assert_eq!(missing.error_code, RPC_ERROR_USER_SKILL_NOT_LISTED);
    }
}
