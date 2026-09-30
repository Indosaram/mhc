//! Port of senpi `packages/tui/src/slash-command-autocomplete.ts`.

use crate::autocomplete::{AutocompleteItem, CommandSpec};
use crate::fuzzy::fuzzy_filter;

const SKILL_COMMAND_PREFIX: &str = "skill:";

#[derive(Debug, Clone)]
struct CommandItem {
    name: String,
    label: String,
    description: Option<String>,
    search_text: String,
}

#[derive(Debug, Clone)]
struct RankedCommandItem {
    value: String,
    label: String,
    description: Option<String>,
    index: usize,
}

fn compare_slash_command_suggestion(prefix: &str, left: &RankedCommandItem, right: &RankedCommandItem) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let left_exact = left.value == prefix;
    let right_exact = right.value == prefix;
    if left_exact != right_exact {
        return if left_exact { Ordering::Less } else { Ordering::Greater };
    }

    let left_prefix = left.value.starts_with(prefix);
    let right_prefix = right.value.starts_with(prefix);
    if left_prefix != right_prefix {
        return if left_prefix { Ordering::Less } else { Ordering::Greater };
    }
    if left_prefix && right_prefix && left.value.len() != right.value.len() {
        return right.value.len().cmp(&left.value.len());
    }

    left.index.cmp(&right.index)
}

pub fn get_slash_command_suggestions(commands: &[CommandSpec], prefix: &str) -> Vec<AutocompleteItem> {
    let normalized_prefix = prefix.to_lowercase();
    let explicit_skill_namespace = normalized_prefix.starts_with(SKILL_COMMAND_PREFIX);
    let has_skill_commands = commands.iter().any(|command| command.name().starts_with(SKILL_COMMAND_PREFIX));

    let mut command_items: Vec<CommandItem> = Vec::new();
    for command in commands {
        let name = command.name();
        let is_skill = name.starts_with(SKILL_COMMAND_PREFIX);
        let skill_name = if is_skill {
            &name[SKILL_COMMAND_PREFIX.len()..]
        } else {
            ""
        };
        if is_skill
            && !explicit_skill_namespace
            && (normalized_prefix.is_empty() || !skill_name.to_lowercase().starts_with(&normalized_prefix))
        {
            continue;
        }

        let hint = command.argument_hint();
        let desc = command.description().unwrap_or("");
        let full_desc = match hint {
            Some(hint) if !hint.is_empty() => {
                if desc.is_empty() {
                    hint.to_string()
                } else {
                    format!("{hint} — {desc}")
                }
            }
            _ => desc.to_string(),
        };
        let description = if full_desc.is_empty() {
            None
        } else {
            Some(full_desc)
        };
        command_items.push(CommandItem {
            name: name.to_string(),
            label: name.to_string(),
            description,
            search_text: if is_skill && !explicit_skill_namespace {
                skill_name.to_string()
            } else {
                name.to_string()
            },
        });
    }

    if has_skill_commands
        && !explicit_skill_namespace
        && !normalized_prefix.is_empty()
        && SKILL_COMMAND_PREFIX.starts_with(&normalized_prefix)
    {
        command_items.push(CommandItem {
            name: SKILL_COMMAND_PREFIX.to_string(),
            label: SKILL_COMMAND_PREFIX.to_string(),
            description: Some("Browse available skills".to_string()),
            search_text: SKILL_COMMAND_PREFIX.to_string(),
        });
    }

    let mut ranked: Vec<RankedCommandItem> = fuzzy_filter(&command_items, prefix, |item| item.search_text.clone())
        .into_iter()
        .enumerate()
        .map(|(index, item)| RankedCommandItem {
            value: item.name,
            label: item.label,
            description: item.description,
            index,
        })
        .collect();
    ranked.sort_by(|left, right| compare_slash_command_suggestion(&normalized_prefix, left, right));
    ranked
        .into_iter()
        .map(|item| AutocompleteItem {
            value: item.value,
            label: item.label,
            description: item.description,
        })
        .collect()
}
