use crate::{
    affinity::{PinSource, UnavailableAccount},
    executable::ExecutableSource,
};
use std::collections::HashSet;
pub static PRESET_APPEND_DEPRECATION: std::sync::LazyLock<
    std::sync::Mutex<PresetAppendDeprecation>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(PresetAppendDeprecation::default()));

#[derive(Default)]
pub struct PresetAppendDeprecation {
    armed_sessions: HashSet<String>,
}
impl PresetAppendDeprecation {
    pub fn reset(&mut self, session: Option<&str>) {
        match session {
            Some(session) => {
                self.armed_sessions.remove(session);
            }
            None => self.armed_sessions.clear(),
        }
    }
    pub fn guidance(&mut self, mode: &str, conflict: bool, session: &str) -> Option<String> {
        if self.armed_sessions.contains(session) || (!conflict && mode != "preset-append") {
            return None;
        }
        self.armed_sessions.insert(session.into());
        let mut parts = Vec::new();
        if mode == "preset-append" {
            parts.push("preset-append system-prompt mode is deprecated; `full` mode delivers the complete senpi system prompt; preset-append will be removed after one release.");
        }
        if conflict {
            parts.push("systemPromptMode wins.");
        }
        Some(parts.join(" "))
    }
}

const PROVIDER: &str = "anthropic-subscription";
const ENABLE_ACCOUNT_FALLBACK: &str = "  Set anthropicSubscriptionProvider.accountFallback: true (or SENPI_CLAUDE_SDK_OAUTH_ACCOUNT_FALLBACK=1) to switch accounts automatically.";

/// How to move off a pinned account. `/claude-account pin` only writes the stored pin, which a CLI or a
/// settings pin outranks, so for those it would change nothing (senpi#3672).
fn switch_account_line(pinned_by: Option<PinSource>) -> &'static str {
    match pinned_by {
        Some(PinSource::Cli) => {
            "  This session was started with --claude-account; start a new session with another --claude-account to switch."
        }
        Some(PinSource::Settings) => {
            "  The pin comes from anthropicSubscriptionProvider.pinnedAccount (or SENPI_CLAUDE_SDK_OAUTH_PINNED_ACCOUNT); change it there to switch accounts."
        }
        Some(PinSource::Stored) | None => {
            "  /claude-account pin <name>  - switch to another account"
        }
    }
}

/// With account fallback off only one account may serve, so the pool-wide advice is wrong. For a blocked
/// account the cause words ("authentication error", "rate limit") stay so the outer credential-pool
/// classifier reads the same verdict; an account that is not configured carries neither on purpose.
pub fn single_account_unavailable_guidance(
    account: &UnavailableAccount,
    soonest_unblock_at: Option<f64>,
    block_reason: Option<&str>,
) -> String {
    if !account.configured {
        let switch = if matches!(
            account.pinned_by,
            Some(PinSource::Cli | PinSource::Settings)
        ) {
            switch_account_line(account.pinned_by).to_owned()
        } else {
            "  /claude-account pin <name>  - pin a configured account (or /claude-account unpin)"
                .to_owned()
        };
        return [format!("Claude account '{}' for {PROVIDER} is not configured, and account fallback is off, so no other account is used.", account.name), "  /claude-account list  - see the configured accounts".into(), switch, "  Start a new session if this session's account was removed.".into(), ENABLE_ACCOUNT_FALLBACK.into()].join("\n");
    }
    let until = soonest_unblock_at
        .filter(|at| at.is_finite())
        .and_then(|at| chrono::DateTime::from_timestamp_millis(at as i64))
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    if block_reason == Some("auth_error") {
        let mut lines = vec![
            format!(
                "Claude account '{}' for {PROVIDER} is blocked (authentication error), and account fallback is off, so no other account is tried.",
                account.name
            ),
            format!("  /login {PROVIDER}  - re-authenticate this account"),
        ];
        if let Some(until) = &until {
            lines.push(format!("  Next automatic re-check: {until}."));
        }
        lines.push(switch_account_line(account.pinned_by).into());
        lines.push(ENABLE_ACCOUNT_FALLBACK.into());
        return lines.join("\n");
    }
    let mut lines = vec![format!(
        "Claude account '{}' for {PROVIDER} is blocked (rate limit){}, and account fallback is off, so no other account is tried.",
        account.name,
        until
            .as_ref()
            .map_or(String::new(), |until| format!(" until {until}"))
    )];
    if let Some(until) = &until {
        lines.push(format!(
            "  Wait until {until}, when this account can serve again."
        ));
    }
    lines.push(switch_account_line(account.pinned_by).into());
    lines.push(ENABLE_ACCOUNT_FALLBACK.into());
    lines.join("\n")
}

pub fn version_floor_guidance(
    text: &str,
    source: Option<ExecutableSource>,
    executable: &str,
) -> Option<String> {
    // PERF-DIVERGENCE(omo anthropic-subscription guidance.ts): upstream uses a regex literal compiled once by the JS engine; this port compiled per call, so we hoist it into a LazyLock. Matches are identical.
    static FLOOR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)does not support this model; version (\S+?) or newer is required|claude_code_version_too_old").expect("version floor"));
    let floor = &*FLOOR;
    if let Some(captures) = floor.captures(text) {
        let target = captures.get(1).map_or_else(
            || "a newer Claude Code".into(),
            |version| {
                format!(
                    "Claude Code {} or newer",
                    version.as_str().trim_end_matches(['.', ',', ';', ':'])
                )
            },
        );
        return Some(match source {
            Some(ExecutableSource::Override) => format!(
                "The Claude Code binary set by CLAUDE_CODE_EXECUTABLE ({executable}) is too old for this model. Replace it with {target}, or unset CLAUDE_CODE_EXECUTABLE to use the Claude Code senpi/omo ships."
            ),
            Some(ExecutableSource::Bundled) => format!(
                "The Claude Code bundled with senpi/omo ({executable}) is too old for this model; `claude update` does not change it. Update senpi/omo, install {target} as `claude` on PATH, or set CLAUDE_CODE_EXECUTABLE to {target} binary."
            ),
            Some(ExecutableSource::Path) => format!(
                "The Claude Code on PATH ({executable}) is too old for this model. Run `claude update` to get {target}, or set CLAUDE_CODE_EXECUTABLE to {target} binary."
            ),
            None => format!(
                "The Claude Code binary is too old for this model. Update senpi/omo, update the `claude` on PATH, or set CLAUDE_CODE_EXECUTABLE to {target} binary."
            ),
        });
    }
    // PERF-DIVERGENCE(omo anthropic-subscription guidance.ts): upstream uses a regex literal compiled once by the JS engine; this port compiled per call, so we hoist it into a LazyLock. Matches are identical.
    static MODEL_MISSING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\bmodel_not_found\b|unrecognized_model|not found for provider").expect("model missing"));
    MODEL_MISSING.is_match(text).then(|| "The bundled Claude Code binary does not know this model id; update senpi/omo or set CLAUDE_CODE_EXECUTABLE to a newer Claude Code binary.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_account_guidance_follows_pin_source_and_cause() {
        let account = |configured, pinned_by| UnavailableAccount {
            name: "work".into(),
            configured,
            pinned_by,
        };
        let cli = single_account_unavailable_guidance(
            &account(true, Some(PinSource::Cli)),
            Some(0.0),
            Some("auth_error"),
        );
        assert!(
            cli.contains("blocked (authentication error)")
                && cli.contains("/login anthropic-subscription")
                && cli.contains("Next automatic re-check: 1970-01-01T00:00:00.000Z.")
                && cli.contains("--claude-account to switch")
        );
        let settings = single_account_unavailable_guidance(
            &account(true, Some(PinSource::Settings)),
            Some(0.0),
            None,
        );
        assert!(
            settings.contains("blocked (rate limit) until 1970-01-01T00:00:00.000Z")
                && settings.contains(
                    "Wait until 1970-01-01T00:00:00.000Z, when this account can serve again."
                )
                && settings.contains("anthropicSubscriptionProvider.pinnedAccount")
        );
        assert!(
            single_account_unavailable_guidance(
                &account(true, Some(PinSource::Stored)),
                None,
                None
            )
            .contains("/claude-account pin <name>  - switch to another account")
        );
        let missing =
            single_account_unavailable_guidance(&account(false, Some(PinSource::Cli)), None, None);
        assert!(
            !missing.contains("authentication error")
                && !missing.contains("rate limit")
                && missing.contains("--claude-account to switch")
                && !missing.contains("or /claude-account unpin")
        );
        assert!(
            single_account_unavailable_guidance(&account(false, None), None, None)
                .contains("or /claude-account unpin")
        );
    }
    #[test]
    fn deprecation_is_once_per_session_and_reset_rearms() {
        let mut state = PresetAppendDeprecation::default();
        assert!(state.guidance("full", false, "s").is_none());
        assert!(state.guidance("preset-append", false, "s").is_some());
        assert!(state.guidance("full", true, "s").is_none());
        assert!(state.guidance("full", true, "other").is_some());
        state.reset(Some("s"));
        assert!(state.guidance("full", true, "s").is_some());
        state.reset(None);
        assert!(state.armed_sessions.is_empty());
    }
}
