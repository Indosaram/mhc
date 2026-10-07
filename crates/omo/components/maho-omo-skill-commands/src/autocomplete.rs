//! Port of `omo-senpi/src/components/skill-commands/autocomplete.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.

use std::collections::BTreeSet;

use maho_omo_bundled_skills::HostCommands;
use maho_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, MentionRange,
};

use crate::bare_skill_command::{
    BareSkillCommandResolution, HostCommandInfo, SKILL_COMMAND_PREFIX, resolve_bare_skill_command,
};

/// Adds the bare `/<bundled-skill>` names next to senpi's `skill:<name>` entries. Every other
/// provider member is the wrapped provider's own, so completion and rendering stay senpi's.
pub struct BareSkillCommandsProvider {
    inner: Box<dyn AutocompleteProvider>,
    bundled_skill_names: BTreeSet<String>,
    host_commands: HostCommands,
}

impl BareSkillCommandsProvider {
    pub fn new(inner: Box<dyn AutocompleteProvider>, bundled_skill_names: BTreeSet<String>, host_commands: HostCommands) -> Self {
        Self { inner, bundled_skill_names, host_commands }
    }
}

pub fn wrap_with_bare_skill_commands(
    current: Box<dyn AutocompleteProvider>,
    bundled_skill_names: BTreeSet<String>,
    host_commands: HostCommands,
) -> Box<dyn AutocompleteProvider> {
    Box::new(BareSkillCommandsProvider::new(current, bundled_skill_names, host_commands))
}

impl AutocompleteProvider for BareSkillCommandsProvider {
    fn trigger_characters(&self) -> Vec<String> {
        self.inner.trigger_characters()
    }

    fn get_suggestions(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let base = self.inner.get_suggestions(lines, cursor_line, cursor_col, force);
        if force || cursor_line != 0 {
            return base;
        }
        let Some(typed) = leading_command_token(lines.first().map(String::as_str).unwrap_or(""), cursor_col) else {
            return base;
        };
        let items = base.as_ref().map(|page| page.items.clone()).unwrap_or_default();
        // Upstream's host-command lookup is a throwing `pi.getCommands?.()`; the native provider
        // trait has no error channel, so a failing lookup degrades to the "no API" case.
        let commands = (self.host_commands)().ok().flatten();
        let aliases = bare_skill_items(&typed, &self.bundled_skill_names, commands.as_deref(), &items);
        if aliases.is_empty() {
            return base;
        }
        let prefix = base.as_ref().map(|page| page.prefix.clone()).unwrap_or_else(|| format!("/{typed}"));
        Some(AutocompleteSuggestions { prefix, items: merge_before_skill_entries(&items, &aliases) })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.inner.apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool {
        self.inner.should_trigger_file_completion(lines, cursor_line, cursor_col)
    }

    fn get_mention_ranges(&self, line: &str) -> Vec<MentionRange> {
        self.inner.get_mention_ranges(line)
    }
}

/// Upstream `LEADING_COMMAND_TOKEN = /^\/([a-z0-9-]+)$/` applied to `(lines[0] ?? "").slice(0, cursorCol)`.
/// JS `slice` clamps past the end of the line, so the length is clamped before slicing.
fn leading_command_token(line: &str, cursor_col: usize) -> Option<String> {
    let rest = line.get(..cursor_col.min(line.len()))?.strip_prefix('/')?;
    if rest.is_empty() || !rest.chars().all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-') {
        return None;
    }
    Some(rest.to_owned())
}

fn bare_skill_items(
    typed: &str,
    bundled_skill_names: &BTreeSet<String>,
    commands: Option<&[HostCommandInfo]>,
    page_items: &[AutocompleteItem],
) -> Vec<AutocompleteItem> {
    // `bundledSkillNames` is already ordered, so upstream's `.sort()` is a no-op here.
    bundled_skill_names
        .iter()
        .filter(|name| name.starts_with(typed))
        .filter(|name| matches!(resolve_bare_skill_command(&format!("/{name}"), bundled_skill_names, commands), BareSkillCommandResolution::Expand { .. }))
        .map(|name| {
            // The alias mirrors its own `skill:<name>` row: senpi puts the skill's argument hint in
            // that row's description. The native `AutocompleteItem` has no `awaitsArguments` field,
            // so only the description is mirrored (parity.d/1007.md records the deviation).
            let skill_command = format!("{SKILL_COMMAND_PREFIX}{name}");
            let skill_row = page_items.iter().find(|item| item.value == skill_command);
            let description = skill_row
                .and_then(|row| row.description.clone())
                .or_else(|| commands.and_then(|commands| commands.iter().find(|entry| entry.name == skill_command).and_then(|entry| entry.description.clone())));
            AutocompleteItem { value: name.clone(), label: name.clone(), description }
        })
        .collect()
}

/// Each alias sits directly above its own `skill:<name>` row, so senpi's ranking of everything else
/// is untouched; an alias whose skill row is absent from this page goes last.
fn merge_before_skill_entries(items: &[AutocompleteItem], aliases: &[AutocompleteItem]) -> Vec<AutocompleteItem> {
    let taken: BTreeSet<&str> = items.iter().map(|item| item.value.as_str()).collect();
    let mut pending: Vec<AutocompleteItem> = aliases.iter().filter(|alias| !taken.contains(alias.value.as_str())).cloned().collect();
    let mut merged = Vec::with_capacity(items.len() + pending.len());
    for item in items {
        if let Some(name) = item.value.strip_prefix(SKILL_COMMAND_PREFIX)
            && let Some(index) = pending.iter().position(|alias| alias.value == name)
        {
            merged.push(pending.remove(index));
        }
        merged.push(item.clone());
    }
    merged.extend(pending);
    merged
}
