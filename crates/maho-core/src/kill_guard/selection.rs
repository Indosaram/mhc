//! Port of `packages/coding-agent/src/core/kill-guard/selection.ts` (senpi#5349).

use super::options::{
    CallerRelativeKind, OptionAction, OptionSpec, OptionTable, SelectingTool, option_table,
    signal_number_or_name,
};
use super::situation::{Situation, dynamic_verdict};
use super::types::KillGuardVerdict;
use crate::bash_command_segments::BashCommandWord;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallerRelative {
    pub option: &'static str,
    pub kind: CallerRelativeKind,
}

/// What one `pkill`/`pgrep`/`killall` call selects, read from its words with the platform's option table.
#[derive(Clone, Debug)]
pub struct Selection {
    pub tool: SelectingTool,
    pub patterns: Vec<String>,
    /// Arguments for the dry-run `pgrep`, without the patterns.
    pub args: Vec<String>,
    pub exact: bool,
    pub regex: bool,
    pub groups: bool,
    pub ancestors: bool,
    pub ignore_ancestors: bool,
    pub ignore_case: bool,
    pub selectors: usize,
    pub signal_zero: bool,
    pub sends_nothing: bool,
    pub unknown_option: Option<String>,
    /// `-g 0` / `-s 0`, resolved against the caller.
    pub caller_relative: Vec<CallerRelative>,
    pub dynamic: Option<KillGuardVerdict>,
}

fn literal(text: String) -> BashCommandWord {
    BashCommandWord {
        literal_prefix: text.clone(),
        text,
        dynamic: false,
        quoted: false,
    }
}

fn is_short_cluster(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else {
        return false;
    };
    rest.len() >= 2 && rest.bytes().all(|b| b.is_ascii_alphabetic())
}

/// A cluster of short flags (`-fx`) as separate words, when every letter is a flag of the table.
fn expand_clusters(words: &[BashCommandWord], table: &OptionTable) -> Vec<BashCommandWord> {
    let mut out = Vec::with_capacity(words.len());
    for (index, word) in words.iter().enumerate() {
        let text = word.text.as_str();
        if word.dynamic || !is_short_cluster(text) || table.contains_key(text) {
            out.push(word.clone());
            continue;
        }
        if index == 0 && signal_number_or_name(&text[1..]).is_some() {
            out.push(word.clone());
            continue;
        }
        let letters: Vec<char> = text[1..].chars().collect();
        let all_valid = letters.iter().enumerate().all(|(position, letter)| {
            let opt = format!("-{letter}");
            table
                .get(opt.as_str())
                .is_some_and(|spec| !spec.takes_value || position == letters.len() - 1)
        });
        if all_valid {
            for letter in letters {
                out.push(literal(format!("-{letter}")));
            }
        } else {
            out.push(word.clone());
        }
    }
    out
}

/// `-u501` / `--euid=501`: an option with its value attached.
fn split_option(text: &str, table: &OptionTable) -> (String, Option<String>) {
    if text.starts_with("--") {
        return match text.split_once('=') {
            Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
            None => (text.to_owned(), None),
        };
    }
    if table.contains_key(text) {
        return (text.to_owned(), None);
    }
    if text.len() >= 2 {
        let mut chars = text.chars();
        if let (Some(first), Some(second)) = (chars.next(), chars.next()) {
            let head: String = [first, second].into_iter().collect();
            if table
                .get(head.as_str())
                .is_some_and(|spec| spec.takes_value)
            {
                let rest: String = chars.collect();
                return (head, Some(rest));
            }
        }
    }
    (text.to_owned(), None)
}

fn forward(selection: &mut Selection, spec: &OptionSpec, short: &'static str, value: Option<&str>) {
    if spec.ignore_ancestors {
        return;
    }
    if selection.tool == SelectingTool::Killall {
        // BSD `killall -c NAME` names the process; `-u` and `-t` select like pgrep's own options.
        if short == "-c"
            && let Some(value) = value
        {
            selection.patterns.push(value.to_owned());
        } else if spec.selector
            && let Some(value) = value
        {
            selection.args.push(short.to_owned());
            selection.args.push(value.to_owned());
        }
        return;
    }
    if let Some(kind) = spec.caller_relative
        && let Some(value) = value
        && value.split(',').any(|part| part == "0")
    {
        selection.caller_relative.push(CallerRelative {
            option: short,
            kind,
        });
        return;
    }
    selection.args.push(short.to_owned());
    if let Some(value) = value {
        selection.args.push(value.to_owned());
    }
}

fn apply_option(
    selection: &mut Selection,
    spec: &OptionSpec,
    short: &'static str,
    value: Option<&str>,
) {
    if spec.signal {
        if let Some(value) = value
            && signal_number_or_name(value).is_some_and(|s| s.zero)
        {
            selection.signal_zero = true;
        }
        return;
    }
    if spec.exact {
        selection.exact = true;
    }
    if spec.regex {
        selection.regex = true;
        selection.exact = false;
    }
    if spec.groups {
        selection.groups = true;
    }
    if spec.ancestors {
        selection.ancestors = true;
    }
    if spec.ignore_ancestors {
        selection.ignore_ancestors = true;
    }
    if spec.ignore_case {
        selection.ignore_case = true;
    }
    if spec.selector {
        selection.selectors += 1;
    }
    match spec.action {
        OptionAction::Allow => {
            selection.sends_nothing = true;
        }
        OptionAction::Unknown => {
            if selection.unknown_option.is_none() {
                selection.unknown_option = Some(short.to_owned());
            }
        }
        OptionAction::Drop | OptionAction::Skip => {}
        OptionAction::Forward => {
            forward(selection, spec, short, value);
        }
    }
}

pub fn read_selection(situation: &Situation<'_>, tool: SelectingTool, platform: &str) -> Selection {
    let table = option_table(tool, platform);
    let mut selection = Selection {
        tool,
        patterns: Vec::new(),
        args: Vec::new(),
        exact: tool == SelectingTool::Killall,
        regex: false,
        groups: false,
        ancestors: false,
        ignore_ancestors: false,
        ignore_case: false,
        selectors: 0,
        signal_zero: false,
        sends_nothing: false,
        unknown_option: None,
        caller_relative: Vec::new(),
        dynamic: None,
    };
    let words = expand_clusters(&situation.segment.words, table);
    let mut options_done = false;
    let mut index = 0usize;
    while index < words.len() {
        let word = &words[index];
        if options_done || !word.literal_prefix.starts_with('-') || word.text == "-" {
            if word.dynamic {
                if selection.dynamic.is_none() {
                    selection.dynamic = Some(dynamic_verdict(situation, "a dynamic pattern"));
                }
            } else {
                selection.patterns.push(word.text.clone());
            }
            index += 1;
            continue;
        }
        if word.dynamic {
            if selection.dynamic.is_none() {
                selection.dynamic = Some(dynamic_verdict(situation, "a dynamic option"));
            }
            index += 1;
            continue;
        }
        if word.text == "--" {
            options_done = true;
            index += 1;
            continue;
        }
        if let Some(signal) = signal_number_or_name(&word.text[1..])
            && (index == 0 || tool == SelectingTool::Killall)
        {
            if signal.zero {
                selection.signal_zero = true;
            }
            index += 1;
            continue;
        }
        let (name, attached) = split_option(&word.text, table);
        let Some((&table_key, &spec)) = table.get_key_value(name.as_str()) else {
            if selection.unknown_option.is_none() {
                selection.unknown_option = Some(word.text.clone());
            }
            index += 1;
            continue;
        };
        let value = if spec.takes_value {
            match attached {
                Some(attached) => Some(literal(attached)),
                None => {
                    let next = words.get(index + 1).cloned();
                    index += 1;
                    next
                }
            }
        } else {
            None
        };
        if value.as_ref().is_some_and(|v| v.dynamic) {
            if selection.dynamic.is_none() {
                selection.dynamic = Some(dynamic_verdict(situation, "a dynamic option value"));
            }
            index += 1;
            continue;
        }
        let short = spec.short.unwrap_or(table_key);
        apply_option(
            &mut selection,
            &spec,
            short,
            value.as_ref().map(|v| v.text.as_str()),
        );
        index += 1;
    }
    selection
}
