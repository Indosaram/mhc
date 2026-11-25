//! Port of senpi `packages/coding-agent/src/modes/rpc/project-skill-commands.ts`.
//!
//! `read_project_skill_file` / `write_project_skill_file`: read and edit the file of a skill the
//! engine lists for the session, so a remote client edits skills like a local one.

use crate::project_file_path::{
    PROJECT_FILE_MAX_BYTES, ProjectFilePath, ResolvedProjectFile, resolve_project_file,
};
use crate::project_file_store::{Stored, etag_of, read_stored, serialize_write};
use crate::project_write_gate::{
    GuardedWrite, ProjectWriteRefusal, WriteAuthorizer, commit_guarded_write, conflict,
    not_allowed, not_trusted, project_trusted_for_write, too_large,
};
use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use maho_core::agent_session::AgentSession;
use maho_core::skills::Skill;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

pub const RPC_ERROR_PROJECT_SKILL_NOT_LISTED: &str = "project_skill_not_listed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectSkillFile {
    pub cwd: String,
    pub skill: String,
    /// The folder-relative path of the skill's file.
    pub path: String,
    pub exists: bool,
    pub content: Option<String>,
    /// Opaque version of the stored bytes; compare it, never parse it.
    pub etag: Option<String>,
    pub project_trusted: bool,
}

pub type ProjectSkillFileResult = Result<RpcProjectSkillFile, ProjectWriteRefusal>;

struct SkillFileState {
    data: RpcProjectSkillFile,
    absolute: PathBuf,
    mode: Option<u32>,
    missing_folders: Vec<PathBuf>,
}

fn not_listed(skill: &str) -> ProjectWriteRefusal {
    ProjectWriteRefusal {
        message: format!("No skill named {skill:?} is listed for this session."),
        error_code: RPC_ERROR_PROJECT_SKILL_NOT_LISTED.to_owned(),
        error_data: Some(json!({ "skill": skill })),
    }
}

pub fn folder_relative(root: &Path, file_path: &Path) -> Option<ProjectFilePath> {
    let mut root_iter = root.components();
    let mut file_iter = file_path.components();
    for root_comp in root_iter.by_ref() {
        if file_iter.next() != Some(root_comp) {
            return None;
        }
    }
    let mut segments = Vec::new();
    for comp in file_iter {
        match comp {
            Component::Normal(os) => {
                let s = os.to_str()?;
                if s.is_empty() || s == "." || s == ".." {
                    return None;
                }
                segments.push(s.to_owned());
            }
            _ => return None,
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(ProjectFilePath {
        relative: segments.join("/"),
        segments,
    })
}

async fn roots_for(path_str: &str) -> Vec<PathBuf> {
    let raw = PathBuf::from(path_str);
    let mut out = HashSet::new();
    out.insert(raw.clone());
    if let Ok(real) = tokio::fs::canonicalize(&raw).await {
        out.insert(maho_core::paths::strip_verbatim_prefix(real));
    }
    out.into_iter().collect()
}

pub async fn locate_skill_file_from_list(
    skills: &[Skill],
    agent_dir: &str,
    cwd: &str,
    skill: &str,
) -> Result<ProjectFilePath, ProjectWriteRefusal> {
    let Some(listed) = skills.iter().find(|candidate| candidate.name == skill) else {
        return Err(not_listed(skill));
    };
    let listed_path = Path::new(&listed.file_path);
    // A skill under the agent dir is global even when that dir lies inside the session folder (e.g. cwd=$HOME).
    for root in roots_for(agent_dir).await {
        if folder_relative(&root, listed_path).is_some() {
            return Err(not_allowed(&listed.file_path));
        }
    }
    for root in roots_for(cwd).await {
        if let Some(file) = folder_relative(&root, listed_path) {
            return Ok(file);
        }
    }
    Err(not_allowed(&listed.file_path))
}

async fn locate_skill_file(
    session: &AgentSession,
    skill: &str,
) -> Result<ProjectFilePath, ProjectWriteRefusal> {
    let skills = session
        .loaded_skills()
        .map(|loaded| loaded.skills)
        .unwrap_or_default();
    locate_skill_file_from_list(&skills, &session.agent_dir(), &session.cwd(), skill).await
}

async fn read_skill_state(
    session: &AgentSession,
    skill: &str,
    file: &ProjectFilePath,
) -> Result<SkillFileState, ProjectWriteRefusal> {
    let cwd = session.cwd();
    let project_trusted = session.with_settings_manager(|s| s.is_project_trusted());
    let resolved = resolve_project_file(Path::new(&cwd), file)
        .await
        .map_err(|_| not_allowed(&file.relative))?;
    let ResolvedProjectFile::Ok {
        absolute,
        exists,
        missing_folders,
    } = resolved
    else {
        return Err(not_allowed(&file.relative));
    };
    let stored = if exists {
        read_stored(&absolute)
            .await
            .map_err(|_| not_allowed(&file.relative))?
    } else {
        Stored::Missing
    };
    match stored {
        Stored::Refused => Err(not_allowed(&file.relative)),
        Stored::TooLarge => Err(too_large()),
        Stored::Missing => Ok(SkillFileState {
            data: RpcProjectSkillFile {
                cwd,
                skill: skill.to_owned(),
                path: file.relative.clone(),
                exists: false,
                content: None,
                etag: None,
                project_trusted,
            },
            absolute,
            mode: None,
            missing_folders,
        }),
        Stored::File { bytes, mode } => Ok(SkillFileState {
            data: RpcProjectSkillFile {
                cwd,
                skill: skill.to_owned(),
                path: file.relative.clone(),
                exists: true,
                content: Some(String::from_utf8_lossy(&bytes).into_owned()),
                etag: Some(etag_of(&bytes)),
                project_trusted,
            },
            absolute,
            mode: Some(mode),
            missing_folders,
        }),
    }
}

pub async fn read_project_skill_file(
    session: &AgentSession,
    skill: Option<&Value>,
) -> ProjectSkillFileResult {
    let Some(skill_name) = skill.and_then(Value::as_str) else {
        return Err(ProjectWriteRefusal {
            message: "read_project_skill_file requires a string 'skill'.".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    let file = locate_skill_file(session, skill_name).await?;
    let state = read_skill_state(session, skill_name, &file).await?;
    Ok(state.data)
}

async fn write_checked(
    session: &AgentSession,
    skill: &str,
    file: &ProjectFilePath,
    content: &str,
    if_etag: Option<&str>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectSkillFileResult {
    let state = read_skill_state(session, skill, file).await?;
    if state.data.etag.as_deref() != if_etag {
        return Err(conflict(&file.relative, state.data.etag.as_deref()));
    }
    if let Some(refusal) = commit_guarded_write(
        session,
        GuardedWrite {
            relative: file.relative.clone(),
            absolute: state.absolute,
            content: content.to_owned(),
            expected_etag: state.data.etag,
            mode: state.mode,
            missing_folders: state.missing_folders,
        },
        authorizer,
    )
    .await
    {
        return Err(refusal);
    }
    let updated = read_skill_state(session, skill, file).await?;
    Ok(updated.data)
}

pub async fn write_project_skill_file(
    session: &AgentSession,
    skill: Option<&Value>,
    content: Option<&Value>,
    if_etag: Option<&Value>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectSkillFileResult {
    let (Some(skill_name), Some(content_str)) = (
        skill.and_then(Value::as_str),
        content.and_then(Value::as_str),
    ) else {
        return Err(ProjectWriteRefusal {
            message: "write_project_skill_file requires a string 'skill', a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    let parsed_etag = match if_etag {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        _ => {
            return Err(ProjectWriteRefusal {
                message: "write_project_skill_file requires a string 'content' and 'ifEtag' (the etag read, or null to create).".to_owned(),
                error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
                error_data: None,
            });
        }
    };
    if !project_trusted_for_write(session).await {
        return Err(not_trusted());
    }
    if content_str.len() > PROJECT_FILE_MAX_BYTES {
        return Err(too_large());
    }
    let file = locate_skill_file(session, skill_name).await?;
    let key = Path::new(&session.cwd()).join(&file.relative);
    serialize_write(key, || {
        write_checked(
            session,
            skill_name,
            &file,
            content_str,
            parsed_etag,
            authorizer,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn locate_skill_excludes_agent_dir_even_inside_home_cwd() {
        let home = tempfile::tempdir().expect("home");
        let agent_dir = home.path().join(".omo/agent");
        let global_skill = agent_dir.join("skills/global/SKILL.md");
        let project_skill = home.path().join(".omo/skills/proj/SKILL.md");
        std::fs::create_dir_all(global_skill.parent().expect("parent")).expect("mkdir");
        std::fs::create_dir_all(project_skill.parent().expect("parent")).expect("mkdir");
        std::fs::write(&global_skill, "g").expect("write");
        std::fs::write(&project_skill, "p").expect("write");

        let skills = vec![
            Skill {
                name: "global".into(),
                file_path: global_skill.display().to_string(),
                ..Default::default()
            },
            Skill {
                name: "proj".into(),
                file_path: project_skill.display().to_string(),
                ..Default::default()
            },
        ];

        let err = locate_skill_file_from_list(
            &skills,
            &agent_dir.display().to_string(),
            &home.path().display().to_string(),
            "global",
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.error_code,
            crate::project_write_gate::RPC_ERROR_PROJECT_FILE_NOT_ALLOWED
        );

        let ok = locate_skill_file_from_list(
            &skills,
            &agent_dir.display().to_string(),
            &home.path().display().to_string(),
            "proj",
        )
        .await
        .expect("project skill");
        assert_eq!(ok.relative, ".omo/skills/proj/SKILL.md");

        let missing = locate_skill_file_from_list(
            &skills,
            &agent_dir.display().to_string(),
            &home.path().display().to_string(),
            "nope",
        )
        .await
        .unwrap_err();
        assert_eq!(missing.error_code, RPC_ERROR_PROJECT_SKILL_NOT_LISTED);
    }
}
