//! Port of `omo-senpi/src/components/skill-commands/bare-skill-command.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.

use std::collections::BTreeSet;
use std::path::Path;

pub use maho_omo_bundled_skills::HostCommandInfo;

pub const SKILL_COMMAND_PREFIX: &str = "skill:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BareSkillCommand {
    pub name: String,
    pub rest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BareSkillCommandResolution {
    NotBareSkill,
    Shadowed,
    Unavailable { name: String },
    Expand { text: String },
}

/// Upstream `BARE_COMMAND_PATTERN = /^\/([a-z0-9][a-z0-9-]*)(?=\s|$)/`: a leading `/name` whose
/// name starts alphanumeric and continues in `[a-z0-9-]`, followed by whitespace or end of input.
pub fn parse_bare_skill_command(text: &str, bundled_skill_names: &BTreeSet<String>) -> Option<BareSkillCommand> {
    let after_slash = text.strip_prefix('/')?;
    let name_len = after_slash
        .find(|character: char| !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'))
        .unwrap_or(after_slash.len());
    let name = &after_slash[..name_len];
    if !name.starts_with(|character: char| character.is_ascii_lowercase() || character.is_ascii_digit()) {
        return None;
    }
    let rest = &after_slash[name_len..];
    if rest.starts_with(|character: char| !character.is_whitespace()) {
        return None;
    }
    if !bundled_skill_names.contains(name) {
        return None;
    }
    Some(BareSkillCommand { name: name.to_owned(), rest: rest.to_owned() })
}

/// Decide what a bare `/<bundled-skill> args` submission becomes.
///
/// `host_commands` is `pi.getCommands()`; `None` means the host predates that API, in which case
/// the rewrite happens unconditionally (senpi leaves an unknown `/skill:` literal, which is no worse
/// than the bare text it replaces).
pub fn resolve_bare_skill_command(
    text: &str,
    bundled_skill_names: &BTreeSet<String>,
    host_commands: Option<&[HostCommandInfo]>,
) -> BareSkillCommandResolution {
    let Some(command) = parse_bare_skill_command(text, bundled_skill_names) else {
        return BareSkillCommandResolution::NotBareSkill;
    };
    if let Some(host_commands) = host_commands {
        // A user prompt template or another extension's command with the same name owns that name.
        if host_commands.iter().any(|entry| entry.name == command.name) {
            return BareSkillCommandResolution::Shadowed;
        }
        let skill_command = format!("{SKILL_COMMAND_PREFIX}{}", command.name);
        if !host_commands.iter().any(|entry| entry.name == skill_command) {
            return BareSkillCommandResolution::Unavailable { name: command.name };
        }
    }
    BareSkillCommandResolution::Expand {
        text: format!("/{SKILL_COMMAND_PREFIX}{}{}", command.name, command.rest),
    }
}

/// Upstream `readBundledSkillNames`: every directory of `skillsDir` holding a `SKILL.md`.
///
/// Upstream throws when an existing directory cannot be listed; the native `Extension::register`
/// cannot return an error, so the port surfaces the failure as `io::Result` and the component
/// aborts registration with a logged diagnostic instead of silently registering nothing.
pub fn read_bundled_skill_names(skills_dir: Option<&Path>) -> std::io::Result<BTreeSet<String>> {
    let Some(skills_dir) = skills_dir.filter(|dir| dir.exists()) else {
        return Ok(BTreeSet::new());
    };
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(skills_dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if skills_dir.join(&name).join("SKILL.md").exists() {
            names.insert(name);
        }
    }
    Ok(names)
}
