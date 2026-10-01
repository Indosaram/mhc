//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/scheduler.ts`.

use std::collections::{HashMap, HashSet};

use super::catalog::types::TipDefinition;

#[derive(Default)]
pub struct SelectTipOptions<'a> {
    pub exclude: Option<&'a HashSet<String>>,
    pub keys: Option<&'a dyn Fn(&str) -> String>,
    pub has_command: Option<&'a dyn Fn(&str) -> bool>,
}

pub fn select_tip<'a>(
    definitions: &[&'a TipDefinition],
    history: &HashMap<String, u64>,
    _now: u64,
    options: SelectTipOptions<'_>,
) -> Option<&'a TipDefinition> {
    let mut oldest_tip: Option<&'a TipDefinition> = None;
    let mut oldest_timestamp = f64::INFINITY;

    for tip in definitions {
        if options.exclude.is_some_and(|exclude| exclude.contains(tip.id)) {
            continue;
        }
        if let Some(keys) = options.keys
            && !tip.bindings.is_empty()
            && tip.bindings.iter().all(|binding| keys(binding).is_empty())
        {
            continue;
        }
        if let Some(has_command) = options.has_command
            && let Some(command) = tip.requires_command
            && !has_command(command)
        {
            continue;
        }
        let Some(last_shown) = history.get(tip.id) else {
            return Some(tip);
        };
        let last_shown = *last_shown as f64;
        if oldest_tip.is_none() || last_shown < oldest_timestamp {
            oldest_tip = Some(tip);
            oldest_timestamp = last_shown;
        }
    }

    oldest_tip
}
