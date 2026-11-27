//! Port of `packages/coding-agent/src/core/kill-guard/selecting.ts` (senpi#5349).

use super::kill::judge_group;
use super::options::{CallerRelativeKind, SelectingTool};
use super::pattern::{PatternMode, PatternVerdict, judge_pattern};
use super::protected::{describe, protected_set, row_of, vantage_chain};
use super::selection::{Selection, read_selection};
use super::situation::Situation;
use super::types::{
    KillGuardContext, KillGuardRule, KillGuardVantage, KillGuardVerdict, ProcessRow, SelectResult,
};
use super::verdict::{ALLOW, NARROW, refuse};
use std::collections::BTreeSet;

fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '^' | '$' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '*' | '+' | '?' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn broad_pattern_hint(
    tool: SelectingTool,
    mode: PatternMode,
    pattern: &str,
    literal_name: bool,
) -> String {
    if mode == PatternMode::Substring
        && pattern.chars().count() == 1
        && tool == SelectingTool::Pkill
    {
        return format!("Match the exact name instead: `pkill -x {pattern}`.");
    }
    if literal_name {
        return NARROW.to_owned();
    }
    "Anchor the pattern to the full command (`pkill -f '^<full command prefix>'`) or match the exact name with `-x`.".to_owned()
}

/// `pkill`, `killall`, and `pgrep` when its output feeds `kill` in the same pipeline (R1, R2, R3, R4, R5).
pub fn judge_selecting(
    situation: &Situation<'_>,
    tool: SelectingTool,
    context: &KillGuardContext,
    piped_into_kill: bool,
) -> KillGuardVerdict {
    if tool == SelectingTool::Pgrep && !piped_into_kill {
        return ALLOW;
    }
    let selection = read_selection(situation, tool, &context.platform);
    if selection.sends_nothing || selection.signal_zero || context.platform == "win32" {
        return ALLOW;
    }
    let mode = if selection.exact {
        PatternMode::Exact
    } else {
        PatternMode::Substring
    };
    for pattern in &selection.patterns {
        if let PatternVerdict::Broad { why } = judge_pattern(pattern, mode) {
            let hint = broad_pattern_hint(
                tool,
                mode,
                pattern,
                tool == SelectingTool::Killall && !selection.regex,
            );
            let quoted_pattern =
                serde_json::to_string(pattern).unwrap_or_else(|_| format!("{pattern:?}"));
            return refuse(
                KillGuardRule::R1,
                &format!("the pattern {quoted_pattern} {why}"),
                Some(&hint),
            );
        }
    }
    if tool == SelectingTool::Pgrep {
        return ALLOW;
    }
    let spelling = &situation.segment.name_spelling;
    if let Some(unknown) = &selection.unknown_option {
        return refuse(
            KillGuardRule::R5,
            &format!("`{spelling}` option {unknown} is not one the guard can resolve"),
            Some("Leave the option out or name the pid."),
        );
    }
    if let Some(dynamic) = selection.dynamic {
        return dynamic;
    }
    if selection.patterns.is_empty() && selection.selectors == 0 {
        return refuse(
            KillGuardRule::R3,
            &format!("`{spelling}` with no name and no selector signals every process it can"),
            Some("Name the process."),
        );
    }
    judge_selection_targets(&selection, context)
}

/// One `pgrep` per killall name: an exact name is matched literally, `-r`/`-m` names as patterns.
fn dry_runs(selection: &Selection, args: &[String]) -> Vec<Vec<String>> {
    if selection.tool != SelectingTool::Killall {
        let mut run = args.to_vec();
        run.extend(selection.patterns.iter().cloned());
        return vec![run];
    }
    let mut base = args.to_vec();
    if selection.ignore_case {
        base.push("-i".to_owned());
    }
    if selection.patterns.is_empty() {
        return vec![base];
    }
    selection
        .patterns
        .iter()
        .map(|name| {
            let mut run = base.clone();
            if selection.regex {
                run.push(name.clone());
            } else {
                run.push("-x".to_owned());
                run.push(escape_regex(name));
            }
            run
        })
        .collect()
}

fn caller_args(
    selection: &Selection,
    context: &KillGuardContext,
    rows: &[ProcessRow],
) -> Result<Vec<String>, KillGuardVerdict> {
    let mut args = selection.args.clone();
    for relative in &selection.caller_relative {
        let pid = match context.vantage {
            KillGuardVantage::Agent => context.agent_pid,
            KillGuardVantage::Process { pid } => pid,
            KillGuardVantage::FreshSession => 0,
        };
        let row = row_of(rows, pid);
        let id = match row {
            None => 0,
            Some(row) => match relative.kind {
                CallerRelativeKind::Pgid => row.pgid,
                CallerRelativeKind::Sid => row.sid,
            },
        };
        if id <= 0 {
            let kind_name = match relative.kind {
                CallerRelativeKind::Pgid => "process group",
                CallerRelativeKind::Sid => "session",
            };
            return Err(refuse(
                KillGuardRule::R5,
                &format!("the caller's {kind_name} cannot be read"),
                None,
            ));
        }
        args.push(relative.option.to_owned());
        args.push(id.to_string());
    }
    Ok(args)
}

fn judge_selection_targets(selection: &Selection, context: &KillGuardContext) -> KillGuardVerdict {
    // `-g 0` / `-s 0` from a fresh shell name only the command's own new group or session.
    if !selection.caller_relative.is_empty() && context.vantage == KillGuardVantage::FreshSession {
        return ALLOW;
    }
    let Some(view) = context.view() else {
        return refuse(
            KillGuardRule::R5,
            "the process table cannot be read, so what it selects cannot be checked",
            None,
        );
    };
    let mut args = match caller_args(selection, context, &view.rows) {
        Ok(args) => args,
        Err(verdict) => return verdict,
    };
    let bsd = context.platform != "linux";
    // The host's own dry run must not drop the host's ancestors, which are the agent's.
    if bsd && !selection.ancestors {
        args.insert(0, "-a".to_owned());
    }
    let mut selected = BTreeSet::new();
    for run in dry_runs(selection, &args) {
        match view.select(&run) {
            SelectResult::Err { reason } => {
                return refuse(
                    KillGuardRule::R5,
                    &format!("its selection cannot be resolved ({reason})"),
                    None,
                );
            }
            SelectResult::Ok { pids } => {
                selected.extend(pids);
            }
        }
    }
    // BSD `pkill` (not `killall`) leaves out its own ancestors unless `-a`; procps does so only with `-A`.
    if selection.ignore_ancestors
        || (bsd && selection.tool == SelectingTool::Pkill && !selection.ancestors)
    {
        for pid in vantage_chain(context, &view.rows) {
            selected.remove(&pid);
        }
    }
    let protected_pids = protected_set(context, &view.rows);
    if selection.groups {
        let mut pgids = BTreeSet::new();
        for &pid in &selected {
            let pgid = row_of(&view.rows, pid).map_or(pid, |row| row.pgid);
            pgids.insert(pgid);
        }
        for pgid in pgids {
            let verdict = judge_group(pgid, &view.rows, &protected_pids);
            if matches!(verdict, KillGuardVerdict::Refuse { .. }) {
                return verdict;
            }
        }
        return ALLOW;
    }
    let hits: Vec<i64> = selected
        .into_iter()
        .filter(|pid| protected_pids.contains_key(pid))
        .collect();
    if hits.is_empty() {
        return ALLOW;
    }
    let named: Vec<String> = hits
        .into_iter()
        .map(|pid| {
            let reason = protected_pids
                .get(&pid)
                .map(String::as_str)
                .unwrap_or("protected");
            describe(pid, Some(&view.rows), reason)
        })
        .collect();
    refuse(
        KillGuardRule::R2,
        &format!("it would signal {}", named.join(", ")),
        None,
    )
}
