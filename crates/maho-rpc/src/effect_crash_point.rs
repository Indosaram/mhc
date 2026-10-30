//! Test-only crash point for the effect ledger's durability tests (senpi `rpc/effect-crash-point.ts`).
//!
//! With `SENPI_TEST_RPC_EFFECT_CRASH=<before|after>:<kind>:<state>` the host SIGKILLs itself right before
//! or right after it appends that `rpc_effect` entry, the only way to prove the reopen seal from a real
//! transcript left by a dead process. Unset in every production launch, so this is a no-op there.

pub const EFFECT_CRASH_ENV: &str = "SENPI_TEST_RPC_EFFECT_CRASH";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectCrashPhase {
    Before,
    After,
}

impl EffectCrashPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// Whether the configured crash point names this append (split out so the match is testable without dying).
pub fn effect_crash_matches(
    configured: Option<&str>,
    phase: EffectCrashPhase,
    kind: &str,
    state: &str,
) -> bool {
    configured == Some(format!("{}:{kind}:{state}", phase.as_str()).as_str())
}

pub fn effect_crash_point(phase: EffectCrashPhase, kind: &str, state: &str) {
    let configured = std::env::var(EFFECT_CRASH_ENV).ok();
    if effect_crash_matches(configured.as_deref(), phase, kind, state) {
        kill_self();
    }
}

#[cfg(unix)]
fn kill_self() {
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
}

#[cfg(not(unix))]
fn kill_self() {
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_phase_kind_and_state_match() {
        let configured = Some("after:input:done");
        assert!(effect_crash_matches(
            configured,
            EffectCrashPhase::After,
            "input",
            "done"
        ));
        assert!(!effect_crash_matches(
            configured,
            EffectCrashPhase::Before,
            "input",
            "done"
        ));
        assert!(!effect_crash_matches(
            configured,
            EffectCrashPhase::After,
            "abort",
            "done"
        ));
        assert!(!effect_crash_matches(
            None,
            EffectCrashPhase::After,
            "input",
            "done"
        ));
    }
}
