//! Port of senpi packages/coding-agent/src/core/skills-prompt.ts.
//!
//! The skills catalog rides on every request, so it is size-bounded: a large catalog is listed with
//! one-line descriptions, then names only, then truncated to whatever fits with an omitted count.
//! Wiring `skills::format_skills_for_prompt` (owned by the senpi-core-ext lane) to this builder is
//! that lane's change.

use crate::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::skills::{FileReadTool, Skill};

pub const COMPACT_SKILL_CATALOG_COUNT: usize = 100;
pub const COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES: usize = 64 * 1024;
pub const MAX_SKILL_CATALOG_BYTES: usize = 48 * 1024;
const ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 160;

/// A catalog this large is listed by name plus a one-line description.
pub fn is_large_skill_catalog(skills: &[&Skill]) -> bool {
    skills.len() > COMPACT_SKILL_CATALOG_COUNT
        || skills
            .iter()
            .map(|skill| skill.description.len())
            .sum::<usize>()
            > COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES
}

pub fn warn_if_large_skill_catalog(skills: &[Skill]) -> Vec<ResourceDiagnostic> {
    let catalog = build_skill_catalog(skills, FileReadTool::Read);
    if !catalog.compact && !catalog.names_only {
        return Vec::new();
    }
    let message = if catalog.names_only {
        format!(
            "The skills section is limited to {} KiB: descriptions were dropped and {} skills were omitted. Remove unused skills to shrink every request.",
            MAX_SKILL_CATALOG_BYTES / 1024,
            catalog.omitted
        )
    } else {
        format!(
            "The skills catalog uses one-line descriptions to limit request size (above {COMPACT_SKILL_CATALOG_COUNT} skills, {} KiB of descriptions, or {} KiB rendered). Remove unused skills to shrink every request.",
            COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES / 1024,
            MAX_SKILL_CATALOG_BYTES / 1024
        )
    };
    vec![ResourceDiagnostic {
        diagnostic_type: ResourceDiagnosticType::Warning,
        category: Some("size-limit".to_owned()),
        message,
        path: None,
        collision: None,
    }]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalog {
    pub prompt: String,
    pub compact: bool,
    pub names_only: bool,
    pub omitted: usize,
}

#[derive(Clone, Copy)]
struct RenderOptions {
    file_read_tool: FileReadTool,
    compact: bool,
    names_only: bool,
    omitted: usize,
}

pub fn build_skill_catalog(skills: &[Skill], file_read_tool: FileReadTool) -> SkillCatalog {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    let compact = is_large_skill_catalog(&visible);
    let options = RenderOptions {
        file_read_tool,
        compact,
        names_only: false,
        omitted: 0,
    };
    let prompt = render_skills(&visible, options);
    if prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt,
            compact,
            names_only: false,
            omitted: 0,
        };
    }
    let compact_prompt = if compact {
        prompt
    } else {
        render_skills(
            &visible,
            RenderOptions {
                compact: true,
                ..options
            },
        )
    };
    if compact_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: compact_prompt,
            compact: true,
            names_only: false,
            omitted: 0,
        };
    }
    let minimal = RenderOptions {
        names_only: true,
        ..options
    };
    let names_prompt = render_skills(&visible, minimal);
    if names_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: names_prompt,
            compact,
            names_only: true,
            omitted: 0,
        };
    }
    let (mut low, mut high) = (0usize, visible.len());
    while low < high {
        let count = (low + high).div_ceil(2);
        let candidate = render_skills(
            &visible[..count],
            RenderOptions {
                omitted: visible.len() - count,
                ..minimal
            },
        );
        if candidate.len() <= MAX_SKILL_CATALOG_BYTES {
            low = count;
        } else {
            high = count - 1;
        }
    }
    let omitted = visible.len() - low;
    SkillCatalog {
        prompt: render_skills(&visible[..low], RenderOptions { omitted, ..minimal }),
        compact,
        names_only: true,
        omitted,
    }
}

/// Format skills for inclusion in a system prompt (Agent Skills XML), within the catalog size budget.
pub fn format_skills_catalog_for_prompt(skills: &[Skill], file_read_tool: FileReadTool) -> String {
    build_skill_catalog(skills, file_read_tool).prompt
}

fn one_line_description(description: &str) -> String {
    let first_line = description.trim().split('\n').next().unwrap_or("").trim();
    if first_line.encode_utf16().count() <= ONE_LINE_DESCRIPTION_MAX_CHARS {
        return first_line.to_owned();
    }
    let cut: String = first_line
        .chars()
        .take(ONE_LINE_DESCRIPTION_MAX_CHARS - 1)
        .collect();
    format!("{}\u{2026}", cut.trim_end())
}

fn skill_root_dir(file_path: &str) -> String {
    let parts: Vec<&str> = file_path.split('/').collect();
    parts[..parts.len().saturating_sub(2)].join("/")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_skills(visible: &[&Skill], options: RenderOptions) -> String {
    if visible.is_empty() && options.omitted == 0 {
        return String::new();
    }
    let mut roots: Vec<(String, String)> = Vec::new();
    for skill in visible {
        let dir = skill_root_dir(&skill.file_path);
        if !roots.iter().any(|(path, _)| *path == dir) {
            let alias = format!("r{}", roots.len());
            roots.push((dir, alias));
        }
    }
    let tool_rule = match options.file_read_tool {
        FileReadTool::Read => {
            "Use the read tool to load a skill's file when its description matches the task and its instructions would change the work; keyword overlap or mere availability is not a reason."
        }
        FileReadTool::Bash => {
            "Use bash to load a skill's file when its description matches the task and its instructions would change the work; keyword overlap or mere availability is not a reason."
        }
    };
    let mut lines: Vec<String> = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        tool_rule.to_owned(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_owned(),
        String::new(),
        "<skill_roots>".to_owned(),
    ];
    for (dir, alias) in &roots {
        lines.push(format!("  <{alias}>{}</{alias}>", escape_xml(dir)));
    }
    lines.push("</skill_roots>".to_owned());
    lines.push("A location's `rN/` prefix expands to the matching root above.".to_owned());
    lines.push(String::new());
    lines.push("<available_skills>".to_owned());
    for skill in visible {
        let dir = skill_root_dir(&skill.file_path);
        let alias = roots
            .iter()
            .find(|(path, _)| *path == dir)
            .map(|(_, alias)| alias.as_str())
            .unwrap_or_default();
        let relative = skill
            .file_path
            .strip_prefix(&format!("{dir}/"))
            .unwrap_or(&skill.file_path);
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        if !options.names_only {
            let description = if options.compact {
                one_line_description(&skill.description)
            } else {
                skill.description.clone()
            };
            lines.push(format!(
                "    <description>{}</description>",
                escape_xml(&description)
            ));
        }
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&format!("{alias}/{relative}"))
        ));
        lines.push("  </skill>".to_owned());
    }
    if options.omitted > 0 {
        lines.push(format!(
            "  <omitted_skills>{}</omitted_skills>",
            options.omitted
        ));
    }
    lines.push("</available_skills>".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_owned(),
            description: description.to_owned(),
            file_path: format!("/skills/{name}/SKILL.md"),
            base_dir: format!("/skills/{name}"),
            source_info: crate::source_info::SourceInfo::default(),
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }
    }

    #[test]
    fn a_small_catalog_keeps_full_descriptions_and_no_warning() {
        let skills = vec![skill("a", "first line\nsecond line")];
        let catalog = build_skill_catalog(&skills, FileReadTool::Read);
        assert!(!catalog.compact && !catalog.names_only);
        assert!(
            catalog
                .prompt
                .contains("<description>first line\nsecond line</description>")
        );
        assert!(catalog.prompt.contains("<r0>/skills</r0>"));
        assert!(
            catalog
                .prompt
                .contains("<location>r0/a/SKILL.md</location>")
        );
        assert!(warn_if_large_skill_catalog(&skills).is_empty());
        assert_eq!(build_skill_catalog(&[], FileReadTool::Bash).prompt, "");
    }

    #[test]
    fn a_large_catalog_goes_compact_then_names_only_with_an_omitted_count() {
        let many: Vec<Skill> = (0..101)
            .map(|i| skill(&format!("s{i}"), "one\ntwo"))
            .collect();
        let catalog = build_skill_catalog(&many, FileReadTool::Read);
        assert!(catalog.compact && !catalog.names_only);
        assert!(catalog.prompt.contains("<description>one</description>"));
        assert_eq!(warn_if_large_skill_catalog(&many).len(), 1);

        let huge: Vec<Skill> = (0..2000)
            .map(|i| skill(&format!("skill-with-a-long-name-{i}"), "d"))
            .collect();
        let catalog = build_skill_catalog(&huge, FileReadTool::Read);
        assert!(catalog.names_only);
        assert!(catalog.omitted > 0);
        assert!(catalog.prompt.len() <= MAX_SKILL_CATALOG_BYTES);
        assert!(catalog.prompt.contains(&format!(
            "<omitted_skills>{}</omitted_skills>",
            catalog.omitted
        )));
        assert!(!catalog.prompt.contains("<description>"));
    }

    #[test]
    fn one_line_descriptions_are_cut_with_an_ellipsis() {
        let long = "x".repeat(200);
        let cut = one_line_description(&long);
        assert_eq!(cut.chars().count(), ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }
}
