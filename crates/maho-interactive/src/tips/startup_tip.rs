//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/startup-tip.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::catalog::types::TipDefinition;
use super::scheduler::{select_tip, SelectTipOptions};

static NEWLINE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\n\s*").expect("valid newline regex"));

pub const TIP_POINTER: &str =
    "↳ Want the full story on any tip? Ask about it — the give-me-tips skill has the tour.";

pub struct StartupTipOptions<'a> {
    pub tips_enabled: bool,
    pub quiet_startup: bool,
    pub history: &'a HashMap<String, u64>,
    pub now: u64,
    pub definitions: &'a [&'static TipDefinition],
    pub keys: &'a dyn Fn(&str) -> String,
    pub has_command: Option<&'a dyn Fn(&str) -> bool>,
    pub exclude: Option<&'a HashSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupTipLine {
    pub line: String,
    pub tip_id: String,
}

pub fn resolve_startup_tip_line(options: StartupTipOptions<'_>) -> Option<StartupTipLine> {
    if !options.tips_enabled {
        return None;
    }
    if options.quiet_startup {
        return None;
    }

    let tip = select_tip(
        options.definitions,
        options.history,
        options.now,
        SelectTipOptions {
            exclude: options.exclude,
            keys: Some(options.keys),
            has_command: options.has_command,
        },
    )?;

    let body = NEWLINE_RUN
        .replace_all(&(tip.render)(options.keys), " ")
        .trim()
        .to_string();
    Some(StartupTipLine {
        line: format!("Tip: {body}\n{TIP_POINTER}"),
        tip_id: tip.id.to_string(),
    })
}
