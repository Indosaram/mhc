//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/working-tip.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::catalog::types::TipDefinition;
use super::scheduler::{select_tip, SelectTipOptions};

static NEWLINE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\n\s*").expect("valid newline regex"));

pub struct WorkingTipOptions<'a> {
    pub tips_enabled: bool,
    pub history: &'a HashMap<String, u64>,
    pub session_shown_tip_ids: &'a HashSet<String>,
    pub now: u64,
    pub definitions: &'a [&'static TipDefinition],
    pub keys: &'a dyn Fn(&str) -> String,
    pub has_command: Option<&'a dyn Fn(&str) -> bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTipLine {
    pub line: String,
    pub tip_id: String,
}

#[derive(Default)]
pub struct WorkingTipCache {
    cached: Option<Option<WorkingTipLine>>,
}

impl WorkingTipCache {
    pub fn new() -> Self {
        Self { cached: None }
    }

    pub fn reset_for_new_turn(&mut self) {
        self.cached = None;
    }

    pub fn resolve(
        &mut self,
        compute: impl FnOnce() -> Option<WorkingTipLine>,
        mut on_first_resolve: Option<&mut dyn FnMut(&WorkingTipLine)>,
    ) -> Option<WorkingTipLine> {
        if let Some(cached) = &self.cached {
            return cached.clone();
        }
        let value = compute();
        self.cached = Some(value.clone());
        if let (Some(value), Some(callback)) = (&value, on_first_resolve.as_mut()) {
            callback(value);
        }
        value
    }
}

pub fn resolve_working_tip_line(options: WorkingTipOptions<'_>) -> Option<WorkingTipLine> {
    if !options.tips_enabled {
        return None;
    }

    let tip = select_tip(
        options.definitions,
        options.history,
        options.now,
        SelectTipOptions {
            exclude: Some(options.session_shown_tip_ids),
            keys: Some(options.keys),
            has_command: options.has_command,
        },
    )?;

    let body = NEWLINE_RUN
        .replace_all(&(tip.render)(options.keys), " ")
        .trim()
        .to_string();
    Some(WorkingTipLine {
        line: format!("Tip: {body}\n{}", super::startup_tip::TIP_POINTER),
        tip_id: tip.id.to_string(),
    })
}
