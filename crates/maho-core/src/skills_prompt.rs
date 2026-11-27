//! Port of senpi packages/coding-agent/src/core/skills-prompt.ts.
//!
//! The skills catalog rides on every request, so it is size-bounded: a large catalog is listed with
//! one-line descriptions, then shorter one-line descriptions, then names only, then truncated to
//! whatever fits with an omitted count.

use std::sync::LazyLock;

use crate::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::dynamic_prompt::pinned_display_path;
use crate::skills::{FileReadTool, Skill};

pub const COMPACT_SKILL_CATALOG_COUNT: usize = 100;
pub const COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES: usize = 16 * 1024;
pub const MAX_SKILL_CATALOG_BYTES: usize = 48 * 1024;
const ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 160;
// A catalog whose 160-character lines overflow the section cap tries shorter lines before dropping descriptions.
const TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 100;
const MIN_FILL_CHARS: usize = 24;

// The when-to-use clause ("Use when...", "MUST USE for...", "Triggers: ...") is what the model selects a skill on.
// An explicit trigger phrase anywhere in the description wins over the generic verb form, which also matches ordinary
// imperative prose ("Run the formatter for each package before tagging."); the verb form is only the fallback.
// Both are anchored at a clause start, which upstream expresses as a lookbehind (`clause_starts`).
static EXPLICIT_TRIGGER_CLAUSE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?:MUST (?:USE|READ|use|read)(?-u:\b)|USE ONLY WHEN(?-u:\b)|Use (?:when|whenever|for)(?-u:\b)|(?:Korean )?[Tt]riggers?(?::| -| on(?-u:\b))|This skill should be used(?-u:\b)|Use this(?-u:\b)|When the user(?-u:\b))",
    )
    .expect("explicit trigger clause")
});
static VERB_TRIGGER_CLAUSE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?:Use|Used|Read|Load|Invoke|Apply|Run|Call)(?-u:\b)[^.!?]{0,40}?(?-u:\b)(?:when|whenever|for|before|after|if|only)(?-u:\b)",
    )
    .expect("verb trigger clause")
});

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
    description_chars: usize,
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
        description_chars: ONE_LINE_DESCRIPTION_MAX_CHARS,
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
    let tight_prompt = render_skills(
        &visible,
        RenderOptions {
            compact: true,
            description_chars: TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS,
            ..options
        },
    );
    if tight_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: tight_prompt,
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

/// `formatSkillsForPrompt`: the Agent Skills XML block, with shared roots aliased, within the catalog size budget.
pub fn format_skills_for_prompt(skills: &[Skill], file_read_tool: FileReadTool) -> String {
    build_skill_catalog(skills, file_read_tool).prompt
}

/// JS `string.length`: descriptions are measured in UTF-16 code units, as upstream does.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `units` UTF-16 code units of `text`, never splitting a character.
fn js_slice(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, character) in text.char_indices() {
        count += character.len_utf16();
        if count > units {
            return &text[..index];
        }
    }
    text
}

/// Byte offsets where upstream's `CLAUSE_START` lookbehind holds: the text start, after `[.!?;:)]` plus whitespace,
/// or after a whitespace-padded dash.
fn clause_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    (0..chars.len()).filter_map(move |position| {
        let starts = position == 0
            || (position >= 2
                && ".!?;:)".contains(chars[position - 2].1)
                && chars[position - 1].1.is_whitespace())
            || (position >= 3
                && chars[position - 3].1.is_whitespace()
                && "-\u{2013}\u{2014}".contains(chars[position - 2].1)
                && chars[position - 1].1.is_whitespace());
        starts.then_some(chars[position].0)
    })
}

fn find_trigger(text: &str, clause: &regex::Regex) -> Option<usize> {
    clause_starts(text).find(|start| clause.is_match(&text[*start..]))
}

// A cut keeps the whole when-to-use sentence (or its start, when that sentence alone is too long) and spends the
// remaining room on the leading what-it-does text, then on whatever follows the trigger sentence.
fn one_line_description(description: &str, max_chars: usize) -> String {
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if js_len(&text) <= max_chars {
        return text;
    }
    let Some(trigger) = find_trigger(&text, &EXPLICIT_TRIGGER_CLAUSE)
        .or_else(|| find_trigger(&text, &VERB_TRIGGER_CLAUSE))
    else {
        return truncate(&text, max_chars);
    };
    let clause_end = text[trigger..]
        .char_indices()
        .find(|(offset, character)| {
            ".!?".contains(*character)
                && text[trigger + offset + 1..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(text.len(), |(offset, _)| trigger + offset + 1);
    let clause = &text[trigger..clause_end];
    if js_len(clause) >= max_chars {
        return truncate(clause, max_chars);
    }
    let mut room = max_chars - js_len(clause);
    let mut fill = |part: &str| -> String {
        if part.is_empty() || room.saturating_sub(1) < MIN_FILL_CHARS {
            return String::new();
        }
        let kept = truncate(part, room - 1);
        room -= js_len(&kept) + 1;
        kept
    };
    let lead = fill(text[..trigger].trim());
    let rest = fill(text[clause_end..].trim());
    [lead.as_str(), clause, rest.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    if js_len(text) <= max_chars {
        return text.to_owned();
    }
    let head = js_slice(text, max_chars - 1);
    let cut = match head.rfind(' ') {
        Some(word_end) if 2 * js_len(&head[..word_end]) > max_chars => &head[..word_end],
        _ => head,
    };
    format!(
        "{}\u{2026}",
        cut.trim_end_matches(
            |character: char| character.is_whitespace() || ",;:".contains(character)
        )
    )
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
        lines.push(format!(
            "  <{alias}>{}</{alias}>",
            escape_xml(&pinned_display_path(dir))
        ));
    }
    let one_line = options.compact || options.names_only;
    lines.push("</skill_roots>".to_owned());
    lines.push(if one_line {
        format!(
            "Each line is `- name [location]{}`. A location `rN` means the file `rN/<name>/SKILL.md`; a longer location is the file's path under that root.",
            if options.names_only {
                ""
            } else {
                ": when to use it"
            }
        )
    } else {
        "A location's `rN/` prefix expands to the matching root above.".to_owned()
    });
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
        if one_line {
            let location = if relative == format!("{}/SKILL.md", skill.name) {
                alias.to_owned()
            } else {
                format!("{alias}/{relative}")
            };
            let head = format!("- {} [{}]", escape_xml(&skill.name), escape_xml(&location));
            lines.push(if options.names_only {
                head
            } else {
                format!(
                    "{head}: {}",
                    escape_xml(&one_line_description(
                        &skill.description,
                        options.description_chars
                    ))
                )
            });
            continue;
        }
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
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
        assert!(catalog.prompt.contains("\n- s0 [r0]: one two\n"));
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
        assert!(!catalog.prompt.contains(": d\n"));
    }

    #[test]
    fn one_line_descriptions_keep_the_trigger_sentence() {
        let long = "x".repeat(200);
        let cut = one_line_description(&long, ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert_eq!(js_len(&cut), ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert!(cut.ends_with('\u{2026}'));

        let description = format!(
            "{} Use when the user asks for a release. {}",
            "Builds and ships artifacts across every supported platform target with signing.",
            "Afterwards it posts notes to every configured channel and archive."
        );
        let cut = one_line_description(&description, TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert!(
            cut.contains("Use when the user asks for a release."),
            "{cut}"
        );
        assert!(
            js_len(&cut) <= TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS,
            "{cut}"
        );
    }
}
