//! Port of `packages/coding-agent/src/core/kill-guard/verdict.ts` (senpi#5349).

use super::types::{KillGuardRule, KillGuardVerdict};

pub const KILL_GUARD_TAG: &str = "[senpi:kill-guard]";

const fn rule_name(rule: KillGuardRule) -> &'static str {
    match rule {
        KillGuardRule::R1 => "BROAD_PATTERN",
        KillGuardRule::R2 => "PROTECTED_TARGET",
        KillGuardRule::R3 => "ALL_PROCESSES",
        KillGuardRule::R4 => "GROUP",
        KillGuardRule::R5 => "UNKNOWN",
    }
}

const OFF_SWITCH: &str =
    "To allow broad kills, set \"terminal.killGuard\": \"off\" in your user settings.";
pub const NARROW: &str = "Signal only what you mean: list it with `pgrep -a -f '^<full command prefix>'` and run `kill <pid>`, use `pkill -x <exact name>`, or stop a senpi job or monitor with kill_bash.";

pub const ALLOW: KillGuardVerdict = KillGuardVerdict::Allow;

fn collapse_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split('\n');
    if let Some(first) = lines.next() {
        out.push_str(first.trim_end());
        for line in lines {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(line.trim_start().trim_end());
        }
    }
    out
}

/// One stderr line: the tag, the rule, the evidence, the narrow alternative and the setting that turns the guard off.
pub fn refuse(rule: KillGuardRule, evidence: &str, alternative: Option<&str>) -> KillGuardVerdict {
    let alt = alternative.unwrap_or(NARROW);
    let line = format!(
        "{KILL_GUARD_TAG} refused ({rule:?} {}): {evidence}. {alt} {OFF_SWITCH}",
        rule_name(rule)
    );
    KillGuardVerdict::Refuse {
        rule,
        message: collapse_newlines(&line),
    }
}

pub fn defer(reason: impl Into<String>) -> KillGuardVerdict {
    KillGuardVerdict::Defer {
        reason: reason.into(),
    }
}

/// The first refusal wins; otherwise a defer; otherwise allow.
pub fn combine<I>(verdicts: I) -> KillGuardVerdict
where
    I: IntoIterator<Item = KillGuardVerdict>,
{
    let mut deferred: Option<KillGuardVerdict> = None;
    for verdict in verdicts {
        if matches!(verdict, KillGuardVerdict::Refuse { .. }) {
            return verdict;
        }
        if matches!(verdict, KillGuardVerdict::Defer { .. }) && deferred.is_none() {
            deferred = Some(verdict);
        }
    }
    deferred.unwrap_or(ALLOW)
}
