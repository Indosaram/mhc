//! Port of `packages/coding-agent/src/core/kill-guard/kill.ts` (senpi#5349).

use super::options::signal_number_or_name;
use super::protected::{Protected, describe, minimal_protected, own_group, protected_set};
use super::situation::{Situation, dynamic_verdict};
use super::types::{
    KillGuardContext, KillGuardRule, KillGuardVantage, KillGuardVerdict, ProcessRow,
};
use super::verdict::{ALLOW, combine, defer, refuse};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillTarget {
    Pid(i64),
    Group(i64),
    All,
}

const ALL_PROCESSES: &str = "a target of `-1` signals every process you may signal";

pub fn refuse_all() -> KillGuardVerdict {
    refuse(KillGuardRule::R3, ALL_PROCESSES, Some("Name the pid."))
}

fn parse_target(text: &str) -> Option<KillTarget> {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        let pid = text.parse::<i64>().ok()?;
        return Some(if pid == 0 {
            KillTarget::Group(0)
        } else {
            KillTarget::Pid(pid)
        });
    }
    if let Some(rest) = text.strip_prefix('-')
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        let pgid = rest.parse::<i64>().ok()?;
        return Some(if pgid == 1 {
            KillTarget::All
        } else {
            KillTarget::Group(pgid)
        });
    }
    None
}

enum SignalHeader {
    Parsed { next: usize, zero: bool },
    Verdict(KillGuardVerdict),
}

fn is_list_or_help_flag(text: &str) -> bool {
    matches!(
        text,
        "-l" | "-L" | "--list" | "--table" | "-h" | "--help" | "-V" | "--version"
    )
}

/// Reads the signal word(s) at the start of a `kill` call: the index of the first target, and whether it sends signal 0.
fn read_signal(situation: &Situation<'_>) -> SignalHeader {
    let words = &situation.segment.words;
    let Some(first) = words.first() else {
        return SignalHeader::Parsed {
            next: 0,
            zero: false,
        };
    };
    let second = words.get(1);
    if first.dynamic {
        if !first.literal_prefix.starts_with('-') {
            return SignalHeader::Parsed {
                next: 0,
                zero: false,
            };
        }
        // `-$SIG`: under a PATH-resetting wrapper its role (signal or group target) is ambiguous (N2).
        if situation.reset_path {
            return SignalHeader::Verdict(dynamic_verdict(
                situation,
                "a dynamic word starting with `-`",
            ));
        }
        return SignalHeader::Parsed {
            next: 1,
            zero: false,
        };
    }
    let text = first.text.as_str();
    if !text.starts_with('-') || text == "--" || text == "-" {
        return SignalHeader::Parsed {
            next: 0,
            zero: false,
        };
    }
    if text == "-s" || text == "-n" || text == "--signal" {
        let zero = second
            .is_some_and(|w| !w.dynamic && signal_number_or_name(&w.text).is_some_and(|s| s.zero));
        return SignalHeader::Parsed { next: 2, zero };
    }
    let name = text.strip_prefix("--signal=").unwrap_or_else(|| &text[1..]);
    let zero = signal_number_or_name(name).is_some_and(|s| s.zero);
    SignalHeader::Parsed { next: 1, zero }
}

/// `kill` (builtin or external): R3 on `-1`, R2 on protected pids, R4 on groups holding a protected process.
pub fn judge_kill(situation: &Situation<'_>, context: &KillGuardContext) -> KillGuardVerdict {
    let words = &situation.segment.words;
    let Some(first) = words.first() else {
        return ALLOW;
    };
    if !first.dynamic && is_list_or_help_flag(&first.text) {
        return ALLOW;
    }
    // `kill -1` alone names signal 1 and no pid; it is refused as the all-processes target it looks like.
    if words.len() == 1 && !first.dynamic && first.text == "-1" {
        return refuse_all();
    }
    let (next, zero) = match read_signal(situation) {
        SignalHeader::Verdict(verdict) => return verdict,
        SignalHeader::Parsed { next, zero } => (next, zero),
    };
    if zero {
        return ALLOW;
    }
    let mut verdicts = Vec::new();
    if first.dynamic && next == 1 && situation.runtime_covers {
        verdicts.push(defer("the signal is only known when the command runs"));
    }
    let mut targets = Vec::new();
    let mut options_done = false;
    for word in words.get(next..).unwrap_or_default() {
        if !options_done && !word.dynamic && word.text == "--" {
            options_done = true;
            continue;
        }
        if word.dynamic {
            verdicts.push(dynamic_target(
                situation,
                word.literal_prefix.starts_with('-'),
            ));
            continue;
        }
        if word.text.starts_with('%') {
            continue;
        }
        if let Some(target) = parse_target(&word.text) {
            targets.push(target);
        }
    }
    if targets.iter().any(|t| matches!(t, KillTarget::All)) {
        return refuse_all();
    }
    if context.platform == "win32" {
        return combine(verdicts);
    }
    let mut all_verdicts = Vec::with_capacity(1 + verdicts.len());
    all_verdicts.push(judge_targets(&targets, context));
    all_verdicts.extend(verdicts);
    combine(all_verdicts)
}

fn dynamic_target(situation: &Situation<'_>, group: bool) -> KillGuardVerdict {
    if group {
        return if situation.runtime_covers {
            defer("a group target is only known when the command runs")
        } else {
            dynamic_verdict(situation, "a dynamic group target")
        };
    }
    // A dynamic positive pid: bash's wrapper sees it later; elsewhere it runs unjudged (A1, residual 1).
    if situation.runtime_covers || situation.reset_path || situation.builtin {
        return defer("the pid is only known when the command runs");
    }
    dynamic_verdict(situation, "a dynamic pid")
}

/// Literal pids and groups, resolved against the caller's vantage.
pub fn judge_targets(targets: &[KillTarget], context: &KillGuardContext) -> KillGuardVerdict {
    if targets.is_empty() {
        return ALLOW;
    }
    let quick = minimal_protected(context);
    for target in targets {
        if let KillTarget::Pid(pid) = *target
            && let Some(reason) = quick.get(&pid)
        {
            return refuse(
                KillGuardRule::R2,
                &format!("it would signal {}", describe(pid, None, reason)),
                None,
            );
        }
    }
    let groups: Vec<i64> = targets
        .iter()
        .filter_map(|t| match *t {
            KillTarget::Group(pgid) => Some(pgid),
            _ => None,
        })
        .collect();
    if groups.contains(&0) && context.vantage == KillGuardVantage::Agent {
        return refuse(
            KillGuardRule::R4,
            &format!(
                "a target of 0 signals the agent's own process group, which holds the agent ({})",
                context.agent_pid
            ),
            None,
        );
    }
    let Some(view) = context.view() else {
        let needs_table = groups
            .iter()
            .any(|&pgid| pgid != 0 || matches!(context.vantage, KillGuardVantage::Process { .. }));
        return if needs_table {
            refuse(
                KillGuardRule::R5,
                "the process table cannot be read, so the process group cannot be checked",
                None,
            )
        } else {
            ALLOW
        };
    };
    let protected_pids = protected_set(context, &view.rows);
    for target in targets {
        if let KillTarget::Pid(pid) = *target
            && let Some(reason) = protected_pids.get(&pid)
        {
            return refuse(
                KillGuardRule::R2,
                &format!(
                    "it would signal {}",
                    describe(pid, Some(&view.rows), reason)
                ),
                None,
            );
        }
    }
    for pgid in groups {
        let resolved = if pgid == 0 {
            own_group(context, &view.rows)
        } else {
            Some(pgid)
        };
        let Some(resolved) = resolved else {
            if pgid == 0 && context.vantage == KillGuardVantage::FreshSession {
                continue;
            }
            return refuse(
                KillGuardRule::R5,
                "the caller's process group cannot be read",
                None,
            );
        };
        let verdict = judge_group(resolved, &view.rows, &protected_pids);
        if matches!(verdict, KillGuardVerdict::Refuse { .. }) {
            return verdict;
        }
    }
    ALLOW
}

pub fn judge_group(pgid: i64, rows: &[ProcessRow], protected_pids: &Protected) -> KillGuardVerdict {
    let mut members: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if row.pgid == pgid
            && let Some(reason) = protected_pids.get(&row.pid)
            && seen.insert(row.pid)
        {
            members.push(describe(row.pid, Some(rows), reason));
        }
    }
    if members.is_empty() {
        return ALLOW;
    }
    refuse(
        KillGuardRule::R4,
        &format!("process group {pgid} holds {}", members.join(", ")),
        None,
    )
}
