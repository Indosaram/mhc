//! Port of senpi `packages/coding-agent/src/core/skills.ts`.

use std::collections::HashSet;
use std::path::Path;

use crate::config::{config_dir_name, get_agent_dir};
use crate::diagnostics::{ResourceCollision, ResourceCollisionType, ResourceDiagnostic, ResourceDiagnosticType};
use crate::frontmatter::parse_frontmatter;
use crate::paths::{PathInputOptions, canonicalize_path, lexical_resolve, resolve_path};
use crate::skill_discovery::{
    IgnoreMatcher, add_ignore_rules, read_skill_markdown_source, should_skip_skill_walk_directory_name,
};
use crate::source_info::{
    SourceInfo, SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};

/// `MAX_NAME_LENGTH` per the Agent Skills spec.
pub const MAX_NAME_LENGTH: usize = 64;
/// `MAX_DESCRIPTION_LENGTH` per the Agent Skills spec.
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// `Skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

/// `LoadSkillsResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// `validateName`.
pub fn validate_name(name: &str) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();

    if name.chars().count() > MAX_NAME_LENGTH {
        errors.push(format!("name exceeds {MAX_NAME_LENGTH} characters ({})", name.chars().count()));
    }

    if !is_valid_name_characters(name) {
        errors.push("name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".to_string());
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }

    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }

    errors
}

fn is_valid_name_characters(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// `validateDescription`.
pub fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();

    match description {
        Some(description) if !description.trim().is_empty() => {
            if description.chars().count() > MAX_DESCRIPTION_LENGTH {
                errors.push(format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                    description.chars().count()
                ));
            }
        }
        _ => errors.push("description is required".to_string()),
    }

    errors
}

/// `LoadSkillsFromDirOptions`.
#[derive(Debug, Clone)]
pub struct LoadSkillsFromDirOptions {
    pub dir: String,
    pub source: String,
}

/// `createSkillSourceInfo`.
pub fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    match source {
        "user" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::User),
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        "project" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::Project),
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        "path" => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
        other => create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: other.to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir.to_string()),
            },
        ),
    }
}

/// `loadSkillsFromDir`.
pub fn load_skills_from_dir(options: &LoadSkillsFromDirOptions) -> LoadSkillsResult {
    load_skills_from_dir_internal(&options.dir, &options.source, true, None, None)
}

fn to_posix_path(path: &str) -> String {
    path.replace(std::path::MAIN_SEPARATOR, "/")
}

fn relative_path(root: &str, path: &str) -> String {
    Path::new(path).strip_prefix(Path::new(root)).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_is(path: &Path) -> Option<bool> {
    let file_type = std::fs::symlink_metadata(path).ok()?.file_type();
    if file_type.is_symlink() {
        Some(std::fs::metadata(path).ok()?.is_file())
    } else {
        Some(file_type.is_file())
    }
}

fn directory_and_file(path: &Path) -> Option<(bool, bool)> {
    let file_type = std::fs::symlink_metadata(path).ok()?.file_type();
    if file_type.is_symlink() {
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.is_dir(), metadata.is_file()))
    } else {
        Some((file_type.is_dir(), file_type.is_file()))
    }
}

fn load_skills_from_dir_internal(
    dir: &str,
    source: &str,
    include_root_files: bool,
    ignore_matcher: Option<&IgnoreMatcher>,
    root_dir: Option<&str>,
) -> LoadSkillsResult {
    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    if !Path::new(dir).exists() {
        return LoadSkillsResult { skills, diagnostics };
    }

    let root = root_dir.unwrap_or(dir).to_string();
    let mut matcher = ignore_matcher.cloned().unwrap_or_default();
    add_ignore_rules(&mut matcher, dir, &root);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return LoadSkillsResult { skills, diagnostics };
    };
    let entries: Vec<std::path::PathBuf> = entries.flatten().map(|entry| entry.path()).collect();

    for full_path in &entries {
        let name = full_path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if name != "SKILL.md" {
            continue;
        }

        let Some(is_file) = file_is(full_path) else {
            continue;
        };
        let rel_path = to_posix_path(&relative_path(&root, &full_path.to_string_lossy()));
        if !is_file || matcher.ignores(&rel_path) {
            continue;
        }

        let (skill, found) = load_skill_from_file(&full_path.to_string_lossy(), source);
        if let Some(skill) = skill {
            skills.push(skill);
        }
        diagnostics.extend(found);
        return LoadSkillsResult { skills, diagnostics };
    }

    for full_path in &entries {
        let name = full_path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if should_skip_skill_walk_directory_name(&name) {
            continue;
        }

        let Some((is_directory, is_file)) = directory_and_file(full_path) else {
            continue;
        };

        let rel_path = to_posix_path(&relative_path(&root, &full_path.to_string_lossy()));
        let ignore_path = if is_directory { format!("{rel_path}/") } else { rel_path.clone() };
        if matcher.ignores(&ignore_path) {
            continue;
        }

        if is_directory {
            let sub_result = load_skills_from_dir_internal(
                &full_path.to_string_lossy(),
                source,
                false,
                Some(&matcher),
                Some(&root),
            );
            skills.extend(sub_result.skills);
            diagnostics.extend(sub_result.diagnostics);
            continue;
        }

        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }

        let (skill, found) = load_skill_from_file(&full_path.to_string_lossy(), source);
        if let Some(skill) = skill {
            skills.push(skill);
        }
        diagnostics.extend(found);
    }

    LoadSkillsResult { skills, diagnostics }
}

fn warning(message: impl Into<String>, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic {
        diagnostic_type: ResourceDiagnosticType::Warning,
        message: message.into(),
        path: Some(path.to_string()),
        collision: None,
    }
}

/// `loadSkillFromFile`.
pub fn load_skill_from_file(file_path: &str, source: &str) -> (Option<Skill>, Vec<ResourceDiagnostic>) {
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let is_declared_skill = Path::new(file_path).file_name().map(|name| name.to_string_lossy() == "SKILL.md").unwrap_or(false);

    let raw_content = match read_skill_markdown_source(file_path) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(warning(error.to_string(), file_path));
            return (None, diagnostics);
        }
    };

    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            if is_declared_skill {
                diagnostics.push(warning(error.message, file_path));
            }
            return (None, diagnostics);
        }
    };

    let description = parsed.frontmatter.get("description").and_then(|value| value.as_str()).map(str::to_string);
    let has_description = description.as_deref().is_some_and(|description| !description.trim().is_empty());
    if !is_declared_skill && !has_description {
        return (None, diagnostics);
    }

    let skill_dir = Path::new(file_path).parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default();
    let parent_dir_name = Path::new(&skill_dir).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();

    for error in validate_description(description.as_deref()) {
        diagnostics.push(warning(error, file_path));
    }

    let frontmatter_name = parsed.frontmatter.get("name").and_then(|value| value.as_str()).map(str::to_string);
    let name = frontmatter_name.filter(|name| !name.is_empty()).unwrap_or(parent_dir_name);

    for error in validate_name(&name) {
        diagnostics.push(warning(error, file_path));
    }

    if !has_description {
        return (None, diagnostics);
    }

    let skill = Skill {
        name,
        description: description.unwrap_or_default(),
        file_path: file_path.to_string(),
        base_dir: skill_dir.clone(),
        source_info: create_skill_source_info(file_path, &skill_dir, source),
        disable_model_invocation: parsed
            .frontmatter
            .get("disable-model-invocation")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    };
    (Some(skill), diagnostics)
}

/// Which tool a session can read skill files with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReadTool {
    Read,
    Bash,
}

/// `formatSkillsForPrompt`: the Agent Skills XML block, with shared roots aliased.
pub fn format_skills_for_prompt(skills: &[Skill], file_read_tool: FileReadTool) -> String {
    let visible_skills: Vec<&Skill> = skills.iter().filter(|skill| !skill.disable_model_invocation).collect();

    if visible_skills.is_empty() {
        return String::new();
    }

    let mut root_by_path: Vec<(String, String)> = Vec::new();
    for skill in &visible_skills {
        let dir = skill_root_dir(&skill.file_path);
        if !root_by_path.iter().any(|(path, _)| *path == dir) {
            let alias = format!("r{}", root_by_path.len());
            root_by_path.push((dir, alias));
        }
    }

    let mut lines: Vec<String> = vec![
        String::new(),
        String::new(),
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        match file_read_tool {
            FileReadTool::Read => "Use the read tool to load a skill's file whenever its description even loosely matches the task - loading an irrelevant skill costs little; missing a relevant one degrades the work.".to_string(),
            FileReadTool::Bash => "Use bash to load a skill's file whenever its description even loosely matches the task - loading an irrelevant skill costs little; missing a relevant one degrades the work.".to_string(),
        },
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<skill_roots>".to_string(),
    ];
    for (dir, alias) in &root_by_path {
        lines.push(format!("  <{alias}>{}</{alias}>", escape_xml(dir)));
    }
    lines.push("</skill_roots>".to_string());
    lines.push("A location's `rN/` prefix expands to the matching root above.".to_string());
    lines.push(String::new());
    lines.push("<available_skills>".to_string());

    for skill in &visible_skills {
        let dir = skill_root_dir(&skill.file_path);
        let alias = root_by_path.iter().find(|(path, _)| *path == dir).map(|(_, alias)| alias.clone()).unwrap_or_default();
        let relative = skill.file_path.strip_prefix(&format!("{dir}/")).unwrap_or(&skill.file_path).to_string();
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!("    <description>{}</description>", escape_xml(&skill.description)));
        lines.push(format!("    <location>{}</location>", escape_xml(&format!("{alias}/{relative}"))));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());

    lines.join("\n")
}

fn skill_root_dir(file_path: &str) -> String {
    let parts: Vec<&str> = file_path.split('/').collect();
    if parts.len() < 2 {
        return String::new();
    }
    parts[..parts.len() - 2].join("/")
}

fn escape_xml(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// `LoadSkillsOptions`.
#[derive(Debug, Clone, Default)]
pub struct LoadSkillsOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub skill_paths: Vec<String>,
    pub include_defaults: bool,
}

/// `loadSkills`: every configured location, with name collisions reported as diagnostics.
pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let process_cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
    let resolved_cwd = resolve_path(&options.cwd, &process_cwd, &PathInputOptions::default());
    let agent_dir = if options.agent_dir.is_empty() { get_agent_dir() } else { options.agent_dir.clone() };
    let resolved_agent_dir = resolve_path(&agent_dir, &process_cwd, &PathInputOptions::default());

    let mut skill_map: Vec<(String, Skill)> = Vec::new();
    let mut real_path_set: HashSet<String> = HashSet::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    let add_skills = |result: LoadSkillsResult, all: &mut Vec<ResourceDiagnostic>, collisions: &mut Vec<ResourceDiagnostic>, map: &mut Vec<(String, Skill)>, seen: &mut HashSet<String>| {
        all.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = canonicalize_path(&skill.file_path);
            if seen.contains(&real_path) {
                continue;
            }
            match map.iter().find(|(name, _)| *name == skill.name) {
                Some((_, existing)) => collisions.push(ResourceDiagnostic {
                    diagnostic_type: ResourceDiagnosticType::Collision,
                    message: format!("name \"{}\" collision", skill.name),
                    path: Some(skill.file_path.clone()),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceCollisionType::Skill,
                        name: skill.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: skill.file_path.clone(),
                        winner_source: None,
                        loser_source: None,
                    }),
                }),
                None => {
                    map.push((skill.name.clone(), skill));
                    seen.insert(real_path);
                }
            }
        }
    };

    if options.include_defaults {
        let user = load_skills_from_dir_internal(
            &Path::new(&resolved_agent_dir).join("skills").to_string_lossy(),
            "user",
            true,
            None,
            None,
        );
        add_skills(user, &mut all_diagnostics, &mut collision_diagnostics, &mut skill_map, &mut real_path_set);
        let project = load_skills_from_dir_internal(
            &lexical_resolve(&Path::new(&resolved_cwd).join(config_dir_name()).join("skills").to_string_lossy()),
            "project",
            true,
            None,
            None,
        );
        add_skills(project, &mut all_diagnostics, &mut collision_diagnostics, &mut skill_map, &mut real_path_set);
    }

    let user_skills_dir = Path::new(&resolved_agent_dir).join("skills").to_string_lossy().into_owned();
    let project_skills_dir = lexical_resolve(&Path::new(&resolved_cwd).join(config_dir_name()).join("skills").to_string_lossy());

    let get_source = |resolved_path: &str| -> String {
        if !options.include_defaults {
            if is_under_path(resolved_path, &user_skills_dir) {
                return "user".to_string();
            }
            if is_under_path(resolved_path, &project_skills_dir) {
                return "project".to_string();
            }
        }
        "path".to_string()
    };

    for raw_path in &options.skill_paths {
        let resolved_path = resolve_path(raw_path, &resolved_cwd, &PathInputOptions { trim: true, ..Default::default() });
        if !Path::new(&resolved_path).exists() {
            all_diagnostics.push(warning("skill path does not exist", &resolved_path));
            continue;
        }

        let Ok(metadata) = std::fs::metadata(&resolved_path) else {
            all_diagnostics.push(warning("failed to read skill path", &resolved_path));
            continue;
        };
        let source = get_source(&resolved_path);
        if metadata.is_dir() {
            let result = load_skills_from_dir_internal(&resolved_path, &source, true, None, None);
            add_skills(result, &mut all_diagnostics, &mut collision_diagnostics, &mut skill_map, &mut real_path_set);
        } else if metadata.is_file() && resolved_path.ends_with(".md") {
            let (skill, diagnostics) = load_skill_from_file(&resolved_path, &source);
            match skill {
                Some(skill) => add_skills(
                    LoadSkillsResult { skills: vec![skill], diagnostics },
                    &mut all_diagnostics,
                    &mut collision_diagnostics,
                    &mut skill_map,
                    &mut real_path_set,
                ),
                None => all_diagnostics.extend(diagnostics),
            }
        } else {
            all_diagnostics.push(warning("skill path is not a markdown file", &resolved_path));
        }
    }

    all_diagnostics.extend(collision_diagnostics);
    LoadSkillsResult { skills: skill_map.into_iter().map(|(_, skill)| skill).collect(), diagnostics: all_diagnostics }
}

fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = lexical_resolve(root);
    if target == normalized_root {
        return true;
    }
    let prefix = if normalized_root.ends_with(std::path::MAIN_SEPARATOR) {
        normalized_root
    } else {
        format!("{normalized_root}{}", std::path::MAIN_SEPARATOR)
    };
    target.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, file_path: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            file_path: file_path.to_string(),
            base_dir: Path::new(file_path).parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default(),
            source_info: create_skill_source_info(file_path, "/skills", "path"),
            disable_model_invocation: false,
        }
    }

    fn write_skill(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn names_must_be_lowercase_hyphenated_and_bounded() {
        assert!(validate_name("demo-skill").is_empty());
        assert_eq!(validate_name("Demo").len(), 1);
        assert_eq!(validate_name("-lead").len(), 1);
        assert_eq!(validate_name("trail-").len(), 1);
        assert_eq!(validate_name("a--b").len(), 1);
        assert_eq!(validate_name("").len(), 1);
        assert_eq!(validate_name(&"a".repeat(MAX_NAME_LENGTH + 1)).len(), 1);
    }

    #[test]
    fn descriptions_are_required_and_bounded() {
        assert!(validate_description(Some("ok")).is_empty());
        assert_eq!(validate_description(Some("   ")), vec!["description is required".to_string()]);
        assert_eq!(validate_description(None), vec!["description is required".to_string()]);
        assert_eq!(validate_description(Some(&"d".repeat(MAX_DESCRIPTION_LENGTH + 1))).len(), 1);
    }

    #[test]
    fn source_scopes_follow_the_source_name() {
        assert_eq!(create_skill_source_info("/s/SKILL.md", "/s", "user").scope, SourceScope::User);
        assert_eq!(create_skill_source_info("/s/SKILL.md", "/s", "project").scope, SourceScope::Project);
        assert_eq!(create_skill_source_info("/s/SKILL.md", "/s", "path").scope, SourceScope::Temporary);
        assert_eq!(create_skill_source_info("/s/SKILL.md", "/s", "auto").source, "auto");
    }

    #[test]
    fn a_declared_skill_without_a_description_warns_and_does_not_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "pack/SKILL.md", "---\nname: pack\n---\nbody");
        let (skill, diagnostics) = load_skill_from_file(&dir.path().join("pack/SKILL.md").to_string_lossy(), "path");
        assert!(skill.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "description is required");
        assert_eq!(diagnostics[0].diagnostic_type, ResourceDiagnosticType::Warning);
    }

    #[test]
    fn a_plain_markdown_file_needs_a_description_to_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "pack/notes.md", "no frontmatter here");
        let (skill, diagnostics) = load_skill_from_file(&dir.path().join("pack/notes.md").to_string_lossy(), "path");
        assert!(skill.is_none());
        assert!(diagnostics.is_empty());

        write_skill(dir.path(), "pack/named.md", "---\ndescription: useful\n---\nbody");
        let (skill, diagnostics) = load_skill_from_file(&dir.path().join("pack/named.md").to_string_lossy(), "path");
        let skill = skill.expect("skill");
        assert_eq!(skill.name, "pack");
        assert_eq!(skill.description, "useful");
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn a_frontmatter_name_overrides_the_directory_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "folder/SKILL.md", "---\nname: custom\ndescription: d\n---\nbody");
        let (skill, _) = load_skill_from_file(&dir.path().join("folder/SKILL.md").to_string_lossy(), "path");
        assert_eq!(skill.expect("skill").name, "custom");
    }

    #[test]
    fn disable_model_invocation_is_read_from_frontmatter() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "pack/SKILL.md", "---\ndescription: d\ndisable-model-invocation: true\n---\nbody");
        let (skill, _) = load_skill_from_file(&dir.path().join("pack/SKILL.md").to_string_lossy(), "path");
        assert!(skill.expect("skill").disable_model_invocation);
    }

    #[test]
    fn a_directory_with_skill_md_is_a_root_and_is_not_walked_further() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "pack/SKILL.md", "---\nname: pack\ndescription: d\n---\nbody");
        write_skill(dir.path(), "pack/nested/SKILL.md", "---\nname: nested\ndescription: d\n---\nbody");
        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir: dir.path().to_string_lossy().into_owned(), source: "path".to_string() });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "pack");
    }

    #[test]
    fn root_markdown_files_load_only_when_the_root_is_scanned() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "skills/top.md", "---\ndescription: top\n---\nbody");
        write_skill(dir.path(), "skills/nested/deep.md", "---\ndescription: deep\n---\nbody");
        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir: dir.path().join("skills").to_string_lossy().into_owned(), source: "path".to_string() });
        let descriptions: Vec<&str> = result.skills.iter().map(|skill| skill.description.as_str()).collect();
        assert_eq!(descriptions, vec!["top"]);

        let nested = load_skills_from_dir(&LoadSkillsFromDirOptions { dir: dir.path().join("skills/nested").to_string_lossy().into_owned(), source: "path".to_string() });
        let nested_descriptions: Vec<&str> = nested.skills.iter().map(|skill| skill.description.as_str()).collect();
        assert_eq!(nested_descriptions, vec!["deep"]);
    }

    #[test]
    fn an_invalid_frontmatter_document_is_skipped_with_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "bad/SKILL.md", "---\ndescription: [unclosed\n---\nbody");
        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir: dir.path().to_string_lossy().into_owned(), source: "path".to_string() });
        assert!(result.skills.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].diagnostic_type, ResourceDiagnosticType::Warning);
        assert!(!result.diagnostics[0].message.is_empty());
        assert_eq!(result.diagnostics[0].path.as_deref(), Some(dir.path().join("bad/SKILL.md").to_string_lossy().as_ref()));
    }

    #[test]
    fn duplicate_names_collide_and_the_first_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "a/SKILL.md", "---\nname: same\ndescription: first\n---\nbody");
        write_skill(dir.path(), "b/SKILL.md", "---\nname: same\ndescription: second\n---\nbody");
        let result = load_skills(&LoadSkillsOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            skill_paths: vec![dir.path().join("a").to_string_lossy().into_owned(), dir.path().join("b").to_string_lossy().into_owned()],
            include_defaults: false,
        });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].description, "first");
        assert_eq!(result.diagnostics.len(), 1);
        let collision = result.diagnostics[0].collision.as_ref().expect("collision");
        assert_eq!(collision.resource_type, ResourceCollisionType::Skill);
        assert_eq!(collision.name, "same");
        assert!(collision.winner_path.ends_with("a/SKILL.md"));
        assert!(collision.loser_path.ends_with("b/SKILL.md"));
    }

    #[test]
    fn a_missing_skill_path_reports_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = load_skills(&LoadSkillsOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            skill_paths: vec![dir.path().join("missing").to_string_lossy().into_owned()],
            include_defaults: false,
        });
        assert!(result.skills.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "skill path does not exist");
    }

    #[test]
    fn defaults_scan_user_then_project_skills() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let cwd = dir.path().join("project");
        write_skill(&agent_dir, "skills/user-skill/SKILL.md", "---\ndescription: u\n---\nbody");
        write_skill(&cwd, &format!("{}/skills/project-skill/SKILL.md", config_dir_name()), "---\ndescription: p\n---\nbody");

        let result = load_skills(&LoadSkillsOptions {
            cwd: cwd.to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            skill_paths: Vec::new(),
            include_defaults: true,
        });
        let names: Vec<&str> = result.skills.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, vec!["user-skill", "project-skill"]);
        assert_eq!(result.skills[0].source_info.scope, SourceScope::User);
        assert_eq!(result.skills[1].source_info.scope, SourceScope::Project);
    }

    #[test]
    fn the_prompt_block_aliases_shared_roots_and_escapes_xml() {
        let skills = vec![
            skill("one", "/root/skills/one/SKILL.md"),
            skill("two", "/root/skills/two/SKILL.md"),
        ];
        let block = format_skills_for_prompt(&skills, FileReadTool::Read);
        assert!(block.starts_with("\n\nThe following skills provide specialized instructions"));
        assert!(block.contains("<r0>/root/skills</r0>"));
        assert!(!block.contains("<r1>"));
        assert!(block.contains("<location>r0/one/SKILL.md</location>"));
        assert!(block.contains("<location>r0/two/SKILL.md</location>"));
        assert!(block.contains("Use the read tool to load a skill's file"));
        assert!(block.ends_with("</available_skills>"));
    }

    #[test]
    fn a_bash_only_tool_set_names_bash_in_the_loading_rule() {
        let skills = vec![skill("one", "/root/skills/one/SKILL.md")];
        let block = format_skills_for_prompt(&skills, FileReadTool::Bash);
        assert!(block.contains("Use bash to load a skill's file"));
    }

    #[test]
    fn hidden_skills_are_excluded_and_an_empty_set_renders_nothing() {
        let mut hidden = skill("hidden", "/root/skills/hidden/SKILL.md");
        hidden.disable_model_invocation = true;
        assert_eq!(format_skills_for_prompt(&[hidden], FileReadTool::Read), "");
        assert_eq!(format_skills_for_prompt(&[], FileReadTool::Read), "");
    }

    #[test]
    fn xml_special_characters_are_escaped_in_every_field() {
        let mut skill = skill("a&b", "/root/skills/x/SKILL.md");
        skill.description = "<tag> \"quoted\" 'single'".to_string();
        let block = format_skills_for_prompt(&[skill], FileReadTool::Read);
        assert!(block.contains("<name>a&amp;b</name>"));
        assert!(block.contains("&lt;tag&gt; &quot;quoted&quot; &apos;single&apos;"));
    }
}
