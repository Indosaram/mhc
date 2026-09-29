//! Committed memory revision compilation into formatted prompt projections.

use std::fmt;

use crate::git::GitMemoryRepo;
use crate::memfs::frontmatter::parse_memory_file;

use super::render::{CompiledSystemFile, render_external_projection, render_system_tree};

const PERSONA_PATH: &str = "system/persona.md";
const IDENTITY_PATH: &str = "system/identity.md";
const REMINDER: &str = "Reminder: <projection> holds local paths of memory projections. <memory> is your persistent memory across conversations. Consult it BEFORE asking the user anything it may already answer. Save durable facts, preferences, decisions, and corrections with the memory tools THE MOMENT they emerge. Route facts about a person to their record under people/ (the primary human's card is system/human.md).";

/// Options for compiling a memory block for a target agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileMemoryBlockOptions {
    pub agent_id: String,
}

/// Errors raised when compiling committed memory into prompt text.
#[derive(Debug)]
pub enum CompileError {
    Git(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(msg) => write!(formatter, "git error compiling memory block: {msg}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile memory block from the HEAD revision of the repository.
pub fn compile_memory_block(
    repo: &GitMemoryRepo,
    options: &CompileMemoryBlockOptions,
) -> Result<String, CompileError> {
    let head = repo.head().map_err(|e| CompileError::Git(e.to_string()))?;
    compile_memory_block_at_revision(repo, head.as_deref(), options)
}

/// Compile memory block at an explicit revision or without revision.
pub fn compile_memory_block_at_revision(
    repo: &GitMemoryRepo,
    revision: Option<&str>,
    options: &CompileMemoryBlockOptions,
) -> Result<String, CompileError> {
    let paths = match revision {
        Some(rev) => repo
            .ls_tree(Some(rev), None)
            .map_err(|e| CompileError::Git(e.to_string()))?,
        None => Vec::new(),
    };

    let persona = match revision {
        Some(rev) if paths.iter().any(|p| p == PERSONA_PATH) => {
            read_system_file(repo, rev, PERSONA_PATH)
        }
        _ => None,
    };

    let identity = match revision {
        Some(rev) if paths.iter().any(|p| p == IDENTITY_PATH) => {
            read_system_file(repo, rev, IDENTITY_PATH)
        }
        _ => None,
    };

    let system_files = match revision {
        Some(rev) => {
            let mut other_paths: Vec<&str> = paths
                .iter()
                .map(String::as_str)
                .filter(|p| is_other_system_markdown(p))
                .collect();
            other_paths.sort_unstable();
            let mut files = Vec::new();
            for p in other_paths {
                if let Some(file) = read_system_file(repo, rev, p) {
                    files.push(file);
                }
            }
            files
        }
        None => Vec::new(),
    };

    let external_paths: Vec<String> = paths.into_iter().filter(|p| is_external_path(p)).collect();

    let projection = render_projection(
        persona.as_ref(),
        identity.as_ref(),
        &system_files,
        &external_paths,
    );
    let metadata = render_metadata(options);

    let parts: Vec<&str> = [projection.as_str(), metadata.as_str()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    Ok(parts.join("\n\n"))
}

fn read_system_file(
    repo: &GitMemoryRepo,
    revision: &str,
    relative_path: &str,
) -> Option<CompiledSystemFile> {
    let content = repo.show(revision, relative_path).ok()?;
    let parsed = parse_memory_file(&content).ok()?;
    Some(CompiledSystemFile {
        relative_path: relative_path.to_string(),
        body: parsed.body,
        description: parsed.frontmatter.description,
    })
}

fn render_projection(
    persona: Option<&CompiledSystemFile>,
    identity: Option<&CompiledSystemFile>,
    system_files: &[CompiledSystemFile],
    external_paths: &[String],
) -> String {
    if persona.is_none()
        && identity.is_none()
        && system_files.is_empty()
        && external_paths.is_empty()
    {
        return String::new();
    }

    let mut lines = vec![REMINDER.to_string()];
    if persona.is_some() || identity.is_some() {
        lines.push(String::new());
        lines.push("<self>".to_string());
        if let Some(file) = persona {
            lines.push("<projection>$MEMORY_DIR/system/persona.md</projection>".to_string());
            let body = file.body.trim_end();
            if !body.is_empty() {
                lines.push(body.to_string());
            }
        }
        if let Some(file) = identity {
            lines.push("<projection>$MEMORY_DIR/system/identity.md</projection>".to_string());
            let body = file.body.trim_end();
            if !body.is_empty() {
                lines.push(body.to_string());
            }
        }
        lines.push("</self>".to_string());
    }

    if !system_files.is_empty() || !external_paths.is_empty() {
        lines.push(String::new());
        lines.push("<memory>".to_string());
        if !system_files.is_empty() {
            lines.push(render_system_tree(system_files));
        }
        if !external_paths.is_empty() {
            lines.push(render_external_projection(external_paths));
        }
        lines.push("</memory>".to_string());
    }

    lines.join("\n")
}

fn is_other_system_markdown(path: &str) -> bool {
    path.starts_with("system/")
        && path != PERSONA_PATH
        && path != IDENTITY_PATH
        && path.ends_with(".md")
}

fn is_external_path(path: &str) -> bool {
    !path.starts_with("system/") && !path.starts_with("skills/")
}

fn render_metadata(options: &CompileMemoryBlockOptions) -> String {
    format!(
        "<memory_metadata>\n- AGENT_ID: {}\n</memory_metadata>",
        options.agent_id
    )
}

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;
