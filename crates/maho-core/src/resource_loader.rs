//! Port of senpi packages/coding-agent/src/core/resource-loader.ts (the project-context and
//! prompt-input layer).
//!
//! DefaultResourceLoader itself is the TS extension and resource-discovery host: extensions are
//! native Rust crates registered statically in maho-cli (plan D-M5), so its extension half has no
//! counterpart here, and the resource half (skills, prompts, themes) is ported in skills.rs,
//! prompt_templates.rs and package_manager.rs. find_git_paths mirrors footer-data-provider.ts,
//! whose git metadata lookup this module needs.

use std::path::Path;

use crate::paths::{canonicalize_path, lexical_resolve, resolve_path, PathInputOptions};
use crate::system_prompt::ContextFile;
use crate::text::strip_bom;

/// senpi loadContextFileFromDir candidates.
pub const AGENTS_FILE_CANDIDATES: [&str; 5] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// senpi resolvePromptInput.
pub fn resolve_prompt_input(input: Option<&str>, description: &str) -> Option<String> {
    let input = input.filter(|input| !input.is_empty())?;

    if Path::new(input).exists() {
        match std::fs::read_to_string(input) {
            Ok(content) => return Some(strip_bom(&content).to_string()),
            Err(error) => {
                eprintln!("Warning: Could not read {description} file {input}: {error}");
                return Some(input.to_string());
            }
        }
    }

    Some(input.to_string())
}

/// senpi loadContextFileFromDir.
pub fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    for filename in AGENTS_FILE_CANDIDATES {
        let file_path = Path::new(dir).join(filename).to_string_lossy().into_owned();
        if !Path::new(&file_path).exists() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&file_path) else {
            eprintln!("Warning: Could not read {file_path}");
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        match std::fs::read_to_string(&file_path) {
            Ok(content) => return Some(ContextFile { path: file_path, content: strip_bom(&content).to_string() }),
            Err(error) => eprintln!("Warning: Could not read {file_path}: {error}"),
        }
    }
    None
}

/// senpi GitPaths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// senpi findGitPaths: walks up from cwd through both a .git directory and a worktree .git file.
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = cwd.to_string();
    loop {
        let git_path = Path::new(&dir).join(".git");
        if git_path.exists() {
            let Ok(metadata) = std::fs::metadata(&git_path) else {
                return None;
            };
            let git_path = git_path.to_string_lossy().into_owned();
            if metadata.is_file() {
                let Ok(content) = std::fs::read_to_string(&git_path) else {
                    return None;
                };
                let content = content.trim().to_string();
                if let Some(target) = content.strip_prefix("gitdir: ") {
                    let git_dir = lexical_resolve(&Path::new(&dir).join(target.trim()).to_string_lossy());
                    let head_path = Path::new(&git_dir).join("HEAD").to_string_lossy().into_owned();
                    if !Path::new(&head_path).exists() {
                        return None;
                    }
                    let common_dir_path = Path::new(&git_dir).join("commondir").to_string_lossy().into_owned();
                    let common_git_dir = match std::fs::read_to_string(&common_dir_path) {
                        Ok(common) => lexical_resolve(&Path::new(&git_dir).join(common.trim()).to_string_lossy()),
                        Err(_) => git_dir,
                    };
                    return Some(GitPaths { repo_dir: dir, common_git_dir, head_path });
                }
            } else if metadata.is_dir() {
                let head_path = Path::new(&git_path).join("HEAD").to_string_lossy().into_owned();
                if !Path::new(&head_path).exists() {
                    return None;
                }
                return Some(GitPaths { repo_dir: dir, common_git_dir: git_path, head_path });
            }
        }
        let parent = Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

/// senpi findShadowedContextFile: the main repo's context file a nested linked worktree shadows.
pub fn find_shadowed_context_file(cwd: &str) -> Option<String> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir);
    let worktree_root = canonicalize_path(&git_paths.repo_dir);
    let main_repo_root = Path::new(&common_git_dir).parent().map(|parent| parent.to_string_lossy().into_owned())?;
    if !worktree_root.starts_with(&format!("{main_repo_root}{}", std::path::MAIN_SEPARATOR)) {
        return None;
    }
    if canonicalize_path(&Path::new(&main_repo_root).join(".git").to_string_lossy()) != common_git_dir {
        return None;
    }
    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    let file_name = Path::new(&worktree_context_file.path).file_name().map(|name| name.to_string_lossy().into_owned())?;
    Some(Path::new(&main_repo_root).join(file_name).to_string_lossy().into_owned())
}

/// senpi loadProjectContextFiles: the agent-dir file first, then the nearest ancestor files.
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let process_cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
    let resolved_cwd = resolve_path(cwd, &process_cwd, &PathInputOptions::default());
    let resolved_agent_dir = resolve_path(agent_dir, &process_cwd, &PathInputOptions::default());

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: Vec<String> = Vec::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.push(global_context.path.clone());
        context_files.push(global_context);
    }

    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();
    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut current_dir = resolved_cwd;

    loop {
        let context_file = load_context_file_from_dir(&current_dir);
        let is_shadowed = shadowed_context_file
            .as_ref()
            .is_some_and(|shadowed| canonicalize_path(context_file.as_ref().map(|file| file.path.as_str()).unwrap_or("")) == *shadowed);
        if let Some(context_file) = context_file
            && !is_shadowed
            && !seen_paths.contains(&context_file.path)
        {
            seen_paths.push(context_file.path.clone());
            ancestor_context_files.insert(0, context_file);
        }

        let parent = Path::new(&current_dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != current_dir => current_dir = parent,
            _ => break,
        }
    }

    context_files.extend(ancestor_context_files);
    context_files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn a_prompt_input_that_names_a_file_is_read_and_stripped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prompt.md");
        write(&path, "\u{feff}file prompt");
        assert_eq!(resolve_prompt_input(Some(&path.to_string_lossy()), "system prompt").as_deref(), Some("file prompt"));
    }

    #[test]
    fn a_literal_prompt_input_passes_through_and_an_empty_one_is_none() {
        assert_eq!(resolve_prompt_input(Some("be brief"), "system prompt").as_deref(), Some("be brief"));
        assert_eq!(resolve_prompt_input(Some(""), "system prompt"), None);
        assert_eq!(resolve_prompt_input(None, "system prompt"), None);
    }

    #[test]
    fn context_files_follow_the_candidate_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("AGENTS.md"), "agents");
        write(&dir.path().join("CLAUDE.md"), "claude");
        let loaded = load_context_file_from_dir(&dir.path().to_string_lossy()).expect("context file");
        assert!(loaded.path.ends_with("AGENTS.md"));
        assert_eq!(loaded.content, "agents");

        write(&dir.path().join("AGENTS.override.md"), "override");
        let loaded = load_context_file_from_dir(&dir.path().to_string_lossy()).expect("context file");
        assert!(loaded.path.ends_with("AGENTS.override.md"));
    }

    #[test]
    fn a_directory_named_like_a_context_file_is_not_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("AGENTS.md")).expect("mkdir");
        assert!(load_context_file_from_dir(&dir.path().to_string_lossy()).is_none());
    }

    #[test]
    fn git_paths_are_found_from_a_regular_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git").join("HEAD"), "ref: refs/heads/main\n");
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let paths = find_git_paths(&nested.to_string_lossy()).expect("git paths");
        assert_eq!(paths.repo_dir, dir.path().to_string_lossy());
        assert!(paths.common_git_dir.ends_with(".git"));
        assert!(paths.head_path.ends_with("HEAD"));
    }

    #[test]
    fn git_paths_are_found_through_a_worktree_git_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git_dir = dir.path().join("main").join(".git").join("worktrees").join("feat");
        write(&git_dir.join("HEAD"), "ref: refs/heads/feat\n");
        let worktree = dir.path().join("feat");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        write(&worktree.join(".git"), &format!("gitdir: {}\n", git_dir.to_string_lossy()));
        let paths = find_git_paths(&worktree.to_string_lossy()).expect("git paths");
        assert_eq!(paths.common_git_dir, git_dir.to_string_lossy());
        assert_eq!(paths.repo_dir, worktree.to_string_lossy());
    }

    #[test]
    fn git_paths_are_absent_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(find_git_paths(&dir.path().to_string_lossy()).is_none());
    }

    #[test]
    fn the_agent_context_file_comes_first_then_ancestors_from_the_root_down() {
        let dir = crate::test_support::isolated_tempdir();
        let agent_dir = dir.path().join("agent");
        let project = dir.path().join("project");
        let nested = project.join("sub");
        write(&agent_dir.join("AGENTS.md"), "agent rules");
        write(&project.join("AGENTS.md"), "project rules");
        write(&nested.join("AGENTS.md"), "nested rules");

        let files = load_project_context_files(&nested.to_string_lossy(), &agent_dir.to_string_lossy());
        let contents: Vec<&str> = files.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(contents, vec!["agent rules", "project rules", "nested rules"]);
    }

    #[test]
    fn a_repeated_context_file_is_loaded_once() {
        let dir = crate::test_support::isolated_tempdir();
        let agent_dir = dir.path().to_string_lossy().into_owned();
        write(&dir.path().join("AGENTS.md"), "rules");
        let files = load_project_context_files(&agent_dir, &agent_dir);
        assert_eq!(files.len(), 1);
    }
}
