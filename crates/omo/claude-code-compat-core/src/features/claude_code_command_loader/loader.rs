use super::{
    loader_cache::{get_cached_commands, get_command_loader_cache_key, set_cached_commands},
    types::*,
};
use crate::shared::logger::log;
use crate::shared::{EXCLUDED_DIRS, get_claude_config_dir};
use futures::FutureExt;
use model_core::{CommandSource, sanitize_model_field};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};
use utils::{
    file_utils::is_markdown_file,
    frontmatter::{FrontmatterMode, parse_frontmatter},
};

pub use super::loader_cache::clear_command_loader_cache;

pub fn load_commands_from_dir(
    directory: &Path,
    scope: CommandScope,
    visited: &mut HashSet<PathBuf>,
    prefix: &str,
) -> Vec<LoadedCommand> {
    if !directory.exists() {
        return Vec::new();
    }
    let realpath = match fs::canonicalize(directory) {
        Ok(path) => path,
        Err(error) => {
            log(
                &format!(
                    "Failed to resolve command directory: {}: {error}",
                    directory.display()
                ),
                None,
            );
            return Vec::new();
        }
    };
    if !visited.insert(realpath) {
        return Vec::new();
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            log(
                &format!(
                    "Failed to read command directory: {}: {error}",
                    directory.display()
                ),
                None,
            );
            return Vec::new();
        }
    };
    let mut commands = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log(
                    &format!(
                        "Failed to read command directory: {}: {error}",
                        directory.display()
                    ),
                    None,
                );
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                log(
                    &format!(
                        "Failed to parse command: {}: {error}",
                        entry.path().display()
                    ),
                    None,
                );
                continue;
            }
        };
        if kind.is_dir() {
            if EXCLUDED_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            let subprefix = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            commands.extend(load_commands_from_dir(
                &entry.path(),
                scope,
                visited,
                &subprefix,
            ));
            continue;
        }
        if !is_markdown_file(&name, kind.is_file()) {
            continue;
        }
        let base_name = name.strip_suffix(".md").unwrap_or(&name);
        let command_name = if prefix.is_empty() {
            base_name.to_owned()
        } else {
            format!("{prefix}/{base_name}")
        };
        let path = entry.path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                log(
                    &format!("Failed to parse command: {}: {error}", path.display()),
                    None,
                );
                continue;
            }
        };
        let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
        let data: CommandFrontmatter = match serde_json::from_value(parsed.data) {
            Ok(data) => data,
            Err(error) => {
                log(
                    &format!("Failed to parse command: {}: {error}", path.display()),
                    None,
                );
                continue;
            }
        };
        let source = match scope {
            CommandScope::User | CommandScope::Project => CommandSource::ClaudeCode,
            CommandScope::Opencode | CommandScope::OpencodeProject => CommandSource::OpenCode,
        };
        let definition = CommandDefinition {
            name: Some(command_name.clone()),
            description: Some(format!(
                "({scope}) {}",
                data.description.unwrap_or_default()
            )),
            template: format!(
                "<command-instruction>\n{}\n</command-instruction>\n\n<user-request>\n$ARGUMENTS\n</user-request>",
                parsed.body.trim()
            ),
            agent: data.agent,
            model: sanitize_model_field(data.model.as_ref(), source),
            subtask: data.subtask,
            argument_hint: data.argument_hint,
            handoffs: data.handoffs,
        };
        commands.push(LoadedCommand {
            name: command_name,
            path,
            definition,
            scope,
        });
    }
    commands
}

pub fn deduplicate_loaded_commands_by_name(commands: Vec<LoadedCommand>) -> Vec<LoadedCommand> {
    let mut seen = HashSet::new();
    commands
        .into_iter()
        .filter(|command| seen.insert(command.name.clone()))
        .collect()
}

pub fn commands_to_record(commands: Vec<LoadedCommand>) -> CommandRecord {
    deduplicate_loaded_commands_by_name(commands)
        .into_iter()
        .map(|command| {
            let mut definition = command.definition;
            definition.name = None;
            definition.argument_hint = None;
            (command.name, definition)
        })
        .collect()
}

pub fn load_user_commands() -> CommandRecord {
    commands_to_record(load_commands_from_dir(
        &get_claude_config_dir().join("commands"),
        CommandScope::User,
        &mut HashSet::new(),
        "",
    ))
}

pub fn load_project_commands(directory: Option<&Path>) -> io::Result<CommandRecord> {
    let directory = match directory {
        Some(directory) => directory.to_path_buf(),
        None => std::env::current_dir()?,
    };
    Ok(commands_to_record(load_commands_from_dir(
        &directory.join(".claude/commands"),
        CommandScope::Project,
        &mut HashSet::new(),
        "",
    )))
}

pub fn load_opencode_global_commands() -> CommandRecord {
    commands_to_record(
        crate::shared::get_open_code_command_dirs(&Default::default())
            .iter()
            .flat_map(|root| {
                load_commands_from_dir(root, CommandScope::Opencode, &mut HashSet::new(), "")
            })
            .collect(),
    )
}

pub fn load_opencode_project_commands(directory: Option<&Path>) -> io::Result<CommandRecord> {
    let root = match directory {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir()?,
    };
    Ok(commands_to_record(
        crate::shared::find_project_opencode_command_dirs(&root, None)
            .iter()
            .flat_map(|root| {
                load_commands_from_dir(root, CommandScope::OpencodeProject, &mut HashSet::new(), "")
            })
            .collect(),
    ))
}

pub async fn load_all_commands(directory: Option<&Path>) -> io::Result<CommandRecord> {
    let key = get_command_loader_cache_key(directory)?;
    if let Some(commands) = get_cached_commands(&key) {
        return Ok(commands.await);
    }
    // Resolve before constructing the shared future; no failed future enters the cache.
    let project = match directory {
        Some(directory) => directory.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let user = get_claude_config_dir().join("commands");
    let commands = async move {
        let mut result = commands_to_record(
            crate::shared::find_project_opencode_command_dirs(&project, None)
                .iter()
                .flat_map(|root| {
                    load_commands_from_dir(
                        root,
                        CommandScope::OpencodeProject,
                        &mut HashSet::new(),
                        "",
                    )
                })
                .collect(),
        );
        result.extend(load_opencode_global_commands());
        result.extend(commands_to_record(load_commands_from_dir(
            &project.join(".claude/commands"),
            CommandScope::Project,
            &mut HashSet::new(),
            "",
        )));
        result.extend(commands_to_record(load_commands_from_dir(
            &user,
            CommandScope::User,
            &mut HashSet::new(),
            "",
        )));
        result
    }
    .boxed()
    .shared();
    set_cached_commands(key, commands.clone());
    Ok(commands.await)
}

#[cfg(test)]
mod tests {
    use super::super::loader_cache::{delete_cached_commands, get_cached_commands};
    use super::*;

    #[test]
    fn global_profile_plural_alias_precedes_parent_and_default_roots() {
        let root = tempfile::tempdir().expect("fixture");
        let parent = root.path().join("opencode");
        let profile = parent.join("profiles/work");
        let xdg = root.path().join("xdg");
        for directory in [
            profile.join("commands"),
            profile.join("command"),
            parent.join("commands"),
            xdg.join("opencode/commands"),
        ] {
            fs::create_dir_all(directory).expect("directory");
        }
        for (directory, agent) in [
            (profile.join("commands"), "profile"),
            (profile.join("command"), "singular"),
            (parent.join("commands"), "parent"),
        ] {
            fs::write(
                directory.join("same.md"),
                format!("---\nagent: {agent}\n---\nrun"),
            )
            .expect("command");
        }
        fs::write(xdg.join("opencode/commands/default.md"), "default").expect("command");
        let _profile = crate::test_support::EnvOverride::set(
            "OPENCODE_CONFIG_DIR",
            &profile.to_string_lossy(),
        );
        let _xdg = crate::test_support::EnvOverride::set("XDG_CONFIG_HOME", &xdg.to_string_lossy());
        let commands = load_opencode_global_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands["same"].agent.as_deref(), Some("profile"));
        assert!(commands.contains_key("default"));
    }

    #[test]
    fn nearest_project_alias_wins_and_worktree_root_bounds_discovery() {
        let root = tempfile::tempdir().expect("fixture");
        let project = root.path().join("repo");
        let child = project.join("packages/app");
        for directory in [
            project.join(".git"),
            root.path().join(".opencode/commands"),
            project.join(".opencode/commands"),
            child.join(".opencode/command/nested"),
        ] {
            fs::create_dir_all(directory).expect("directory");
        }
        fs::write(root.path().join(".opencode/commands/outside.md"), "outside").expect("command");
        fs::write(
            project.join(".opencode/commands/same.md"),
            "---\nmodel: openai/gpt-5.5\n---\nparent",
        )
        .expect("command");
        fs::write(
            child.join(".opencode/command/same.md"),
            "---\nagent: nearest\nmodel: openai/gpt-5.5\n---\nchild",
        )
        .expect("command");
        fs::write(child.join(".opencode/command/nested/run.md"), "nested").expect("command");
        let commands = load_opencode_project_commands(Some(&child)).expect("commands");
        assert_eq!(commands.len(), 2);
        assert_eq!(commands["same"].agent.as_deref(), Some("nearest"));
        assert_eq!(commands["same"].model.as_deref(), Some("openai/gpt-5.5"));
        assert!(commands.contains_key("nested/run"));
        assert!(!commands.contains_key("outside"));
    }

    #[test]
    fn recursive_commands_skip_excluded_directories_and_preserve_fields() {
        let root = tempfile::tempdir().expect("fixture directory");
        let directory = root.path().join(".claude/commands");
        for name in ["deploy", "node_modules", ".hidden", "target"] {
            fs::create_dir_all(directory.join(name)).expect("command directory");
            fs::write(directory.join(name).join("run.md"), "---\ndescription: Run\nargument-hint: target\nagent: worker\nmodel: opus\nsubtask: true\nhandoffs:\n  - label: Next\n    agent: next\n    prompt: Continue\n    send: false\n---\n Do it. \n").expect("fixture write");
        }
        let loaded =
            load_commands_from_dir(&directory, CommandScope::Project, &mut HashSet::new(), "");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "deploy/run");
        assert_eq!(
            loaded[0].definition.argument_hint.as_deref(),
            Some("target")
        );
        let commands = load_project_commands(Some(root.path())).expect("commands");
        let command = &commands["deploy/run"];
        assert_eq!(command.description.as_deref(), Some("(project) Run"));
        assert_eq!(
            command.template,
            "<command-instruction>\nDo it.\n</command-instruction>\n\n<user-request>\n$ARGUMENTS\n</user-request>"
        );
        assert_eq!(command.agent.as_deref(), Some("worker"));
        assert_eq!(command.subtask, Some(true));
        assert_eq!(
            command.handoffs.as_ref().expect("handoffs")[0].send,
            Some(false)
        );
        assert!(command.model.is_none());
        assert!(command.name.is_none());
        assert!(command.argument_hint.is_none());
    }

    #[test]
    fn cached_directory_is_reused_after_source_changes() {
        let root = tempfile::tempdir().expect("fixture directory");
        let directory = root.path().join(".claude/commands");
        fs::create_dir_all(&directory).expect("command directory");
        let path = directory.join("cached.md");
        fs::write(&path, "Original.").expect("fixture write");
        let first = futures::executor::block_on(load_all_commands(Some(root.path())))
            .expect("first commands");
        fs::write(&path, "Changed.").expect("fixture rewrite");
        let second = futures::executor::block_on(load_all_commands(Some(root.path())))
            .expect("cached commands");
        assert_eq!(second, first);
        assert!(second["cached"].template.contains("Original."));
        let key = get_command_loader_cache_key(Some(root.path())).expect("key");
        delete_cached_commands(&key);
        assert!(get_cached_commands(&key).is_none());
    }

    #[test]
    fn missing_directories_and_revisited_realpaths_are_empty() {
        let root = tempfile::tempdir().expect("fixture directory");
        let missing = root.path().join("missing-commands");
        assert_eq!(
            get_command_loader_cache_key(Some(&missing)).expect("key"),
            missing
        );
        assert!(
            load_project_commands(Some(root.path()))
                .expect("commands")
                .is_empty()
        );
        fs::write(root.path().join("real.md"), "Prompt.").expect("fixture write");
        let mut visited = HashSet::from([fs::canonicalize(root.path()).expect("realpath")]);
        assert!(
            load_commands_from_dir(root.path(), CommandScope::User, &mut visited, "").is_empty()
        );
    }

    #[test]
    fn duplicate_command_names_keep_first_definition() {
        let commands = vec!["First", "Second"]
            .into_iter()
            .map(|description| LoadedCommand {
                name: "duplicate".into(),
                path: PathBuf::from("command.md"),
                scope: CommandScope::User,
                definition: CommandDefinition {
                    description: Some(description.into()),
                    ..CommandDefinition::default()
                },
            })
            .collect();
        assert_eq!(
            commands_to_record(commands)["duplicate"]
                .description
                .as_deref(),
            Some("First")
        );
    }
}
