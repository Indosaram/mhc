//! The `skill` tool and the `/skill` command text (spec §4, §5).

use maho_ext_api::{Skill, SourceScope};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

/// Tool name.
pub const TOOL_NAME: &str = "skill";
/// Largest skill-relative file the tool returns.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// What a `skill` call produced: model-facing text, error flag and details.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub details: Value,
}

impl SkillToolOutcome {
    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            details: Value::Null,
        }
    }
}

/// Drops a leading YAML front matter block (`---` … `---`) and the blank lines after it.
pub fn strip_front_matter(content: &str) -> &str {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        return content;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return rest[offset..].trim_start_matches(['\n', '\r']);
        }
    }
    content
}

fn model_visible<'a>(skills: &'a [Skill], disabled: &[String]) -> Vec<&'a str> {
    skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation && !disabled.contains(&skill.name))
        .map(|skill| skill.name.as_str())
        .collect()
}

fn unknown(name: &str, skills: &[Skill], disabled: &[String]) -> SkillToolOutcome {
    let available = model_visible(skills, disabled);
    let available = if available.is_empty() {
        "none".to_owned()
    } else {
        available.join(", ")
    };
    SkillToolOutcome::error(format!("Unknown skill: {name}\nAvailable: {available}"))
}

fn check_relative(relative: &str) -> Result<(), String> {
    let path = Path::new(relative);
    let windows_drive = relative.as_bytes().get(1) == Some(&b':');
    if path.is_absolute() || relative.starts_with(['/', '\\']) || windows_drive {
        return Err("Absolute paths are not allowed in skill paths".into());
    }
    if relative.split(['/', '\\']).any(|segment| segment == "..")
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err("Path traversal (..) is not allowed in skill paths".into());
    }
    Ok(())
}

fn read_relative(skill: &Skill, relative: &str) -> SkillToolOutcome {
    if let Err(error) = check_relative(relative) {
        return SkillToolOutcome::error(error);
    }
    let base = PathBuf::from(&skill.base_dir);
    let target = base.join(relative);
    let label = format!("{}/{}", skill.name, relative.trim_end_matches(['/', '\\']));
    let (Ok(real_base), Ok(real_target)) = (base.canonicalize(), target.canonicalize()) else {
        return SkillToolOutcome::error(format!("Not found: {label}"));
    };
    if !real_target.starts_with(&real_base) {
        return SkillToolOutcome::error("Path resolves outside the skill directory");
    }
    let details = json!({"name": skill.name, "path": real_target.to_string_lossy(), "baseDir": skill.base_dir});
    if real_target.is_dir() {
        let mut entries: Vec<String> = match std::fs::read_dir(&real_target) {
            Ok(read) => read
                .filter_map(Result::ok)
                .map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if entry.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect(),
            Err(error) => return SkillToolOutcome::error(format!("Cannot list {label}: {error}")),
        };
        entries.sort();
        return SkillToolOutcome {
            text: format!("[{label}/]\n{}", entries.join("\n")),
            is_error: false,
            details,
        };
    }
    match std::fs::metadata(&real_target) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => {
            return SkillToolOutcome::error(format!(
                "{label} is {} bytes; the skill tool returns files up to {MAX_FILE_BYTES} bytes (use read for larger files)",
                meta.len()
            ));
        }
        Ok(_) => {}
        Err(error) => return SkillToolOutcome::error(format!("Cannot read {label}: {error}")),
    }
    match std::fs::read(&real_target) {
        Ok(bytes) => SkillToolOutcome {
            text: format!("[{label}]\n{}", String::from_utf8_lossy(&bytes)),
            is_error: false,
            details,
        },
        Err(error) => SkillToolOutcome::error(format!("Cannot read {label}: {error}")),
    }
}

/// Runs a `skill` call: `params.name` (`<skill>` or `<skill>/<relative path>`) and optional
/// `params.arguments`, against the session's loaded `skills` and the `disabled` denylist.
pub fn run_skill_tool(skills: &[Skill], disabled: &[String], params: &Value) -> SkillToolOutcome {
    let Some(requested) = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return SkillToolOutcome::error("skill: `name` is required");
    };
    let (name, relative) = match requested.split_once('/') {
        Some((name, relative)) => (name, Some(relative)),
        None => (requested, None),
    };
    let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
        return unknown(name, skills, disabled);
    };
    if disabled.iter().any(|entry| entry == name) {
        return SkillToolOutcome::error(format!(
            "Skill \"{name}\" is disabled by configuration (disabled_skills)."
        ));
    }
    if skill.disable_model_invocation {
        return SkillToolOutcome::error(format!(
            "Skill \"{name}\" can only be invoked by the user (/skill:{name})."
        ));
    }
    if let Some(relative) = relative.filter(|relative| !relative.is_empty()) {
        return read_relative(skill, relative);
    }
    let content = match std::fs::read_to_string(&skill.file_path) {
        Ok(content) => content,
        Err(error) => {
            return SkillToolOutcome::error(format!(
                "Cannot read skill \"{name}\" at {}: {error}",
                skill.file_path
            ));
        }
    };
    let mut text = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        skill.base_dir,
        strip_front_matter(&content).trim_end()
    );
    if let Some(arguments) = params
        .get("arguments")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|arguments| !arguments.is_empty())
    {
        text.push_str("\n\nUser: ");
        text.push_str(arguments);
    }
    SkillToolOutcome {
        text,
        is_error: false,
        details: json!({"name": skill.name, "path": skill.file_path, "baseDir": skill.base_dir}),
    }
}

fn scope_label(skill: &Skill, bundled_root: Option<&Path>) -> String {
    if bundled_root.is_some_and(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let file = Path::new(&skill.file_path);
        file.canonicalize()
            .unwrap_or_else(|_| file.to_path_buf())
            .starts_with(root)
    }) {
        return "bundled".into();
    }
    match skill.source_info.scope {
        SourceScope::User => "user".into(),
        SourceScope::Project => "project".into(),
        SourceScope::Temporary => "path".into(),
        SourceScope::System if !skill.source_info.source.is_empty() => {
            skill.source_info.source.clone()
        }
        SourceScope::System => "system".into(),
    }
}

fn flags(skill: &Skill, disabled: &[String]) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if disabled.contains(&skill.name) {
        flags.push("disabled");
    }
    if skill.disable_model_invocation {
        flags.push("user-only");
    }
    flags
}

/// `/skill` without an argument: one line per loaded skill.
pub fn list_text(skills: &[Skill], disabled: &[String], bundled_root: Option<&Path>) -> String {
    if skills.is_empty() {
        return "No skills loaded.".into();
    }
    let mut lines = vec![format!("Skills ({}):", skills.len())];
    for skill in skills {
        let flags = flags(skill, disabled);
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        lines.push(format!(
            "  {} ({}){} - {}",
            skill.name,
            scope_label(skill, bundled_root),
            flags,
            skill.file_path
        ));
    }
    lines.join("\n")
}

/// `/skill <name>`: description, location, status and how to run it.
pub fn detail_text(
    skills: &[Skill],
    disabled: &[String],
    bundled_root: Option<&Path>,
    name: &str,
) -> String {
    let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
        let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
        let names = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        };
        return format!("Unknown skill: {name}\nAvailable: {names}");
    };
    let flags = flags(skill, disabled);
    let status = if flags.is_empty() {
        "enabled".to_owned()
    } else {
        flags.join(", ")
    };
    format!(
        "{}\n{}\nScope: {}\nStatus: {}\nFile: {}\nDirectory: {}\nRun: /skill:{} [instructions]",
        skill.name,
        skill.description,
        scope_label(skill, bundled_root),
        status,
        skill.file_path,
        skill.base_dir,
        skill.name
    )
}
