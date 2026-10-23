pub const TERMINAL_BASH_TOOL: &str = "bash";
pub const TERMINAL_OUTPUT_TOOL: &str = "bash_output";
pub const TERMINAL_KILL_TOOL: &str = "kill_bash";
pub const TERMINAL_INPUT_TOOL: &str = "bash_input";
pub const TERMINAL_RESIZE_TOOL: &str = "bash_resize";
pub const TERMINAL_MONITOR_TOOL: &str = "monitor";
pub const TERMINAL_MONITOR_STATE_EVENT: &str = "terminal_monitor_state";
pub const TERMINAL_MONITOR_ENDED_EVENT: &str = "terminal_monitor_ended";
pub const WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const TERMINAL_COMPANION_TOOLS: &[&str] = &[
    TERMINAL_OUTPUT_TOOL,
    TERMINAL_KILL_TOOL,
    TERMINAL_INPUT_TOOL,
    TERMINAL_RESIZE_TOOL,
    TERMINAL_MONITOR_TOOL,
];
pub const DEFAULT_COLS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 40;
pub const DEFAULT_SCROLLBACK: usize = 10_000;
pub const DEFAULT_MAX_SESSIONS: usize = 32;
pub const MAX_SESSION_OUTPUT_CHARS: usize = 1_000_000;
pub const BACKGROUND_START_GRACE_MS: u64 = 250;
pub const KILLED_SESSION_EXIT_GRACE_MS: u64 = 5000;
pub const MAX_DURABLE_MONITORS: usize = 5;
pub const DURABLE_MONITOR_EXPIRY_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const FIRE_BUDGET_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// Wake pacing of a persistent command watch: average wakes an hour (and burst size); extra lines batch.
pub const DEFAULT_MONITOR_WAKES_PER_HOUR: usize = 12;
/// Runaway guard: matched lines a minute that count as a flood minute...
pub const DEFAULT_MONITOR_RUNAWAY_LINES_PER_MINUTE: usize = 120;
/// ...for this many consecutive minutes pauses the watch until it is re-armed.
pub const DEFAULT_MONITOR_RUNAWAY_MINUTES: usize = 5;
/// One per-monitor overflow file grows to this size before it rotates to a single `.1` copy.
pub const MONITOR_OVERFLOW_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Machine-consumed prefix of the runaway pause summary: a pause, not a completion.
pub const RUNAWAY_PAUSE_SUMMARY_PREFIX: &str = "paused: runaway output";

pub fn runaway_pause_summary(bash_id: &str, lines_per_minute: usize, minutes: usize) -> String {
    format!(
        "{RUNAWAY_PAUSE_SUMMARY_PREFIX} (more than {lines_per_minute} matched lines a minute for {minutes} minute{}); later lines stay in bash_output. Resume with monitor({{ action: \"rearm\", bash_id: \"{bash_id}\" }}).",
        if minutes == 1 { "" } else { "s" }
    )
}
pub const FOREGROUND_ENV_OVERRIDES: &[(&str, &str)] = &[
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("GIT_EDITOR", "true"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

pub fn resolve_key_sequence(key: &str) -> Option<&str> {
    let normalized = key.trim().to_lowercase();
    match normalized.as_str() {
        "enter" | "return" => Some("\r"),
        "tab" => Some("\t"),
        "escape" | "esc" => Some("\x1b"),
        "space" => Some(" "),
        "backspace" => Some("\x7f"),
        "delete" => Some("\x1b[3~"),
        "up" => Some("\x1b[A"),
        "down" => Some("\x1b[B"),
        "right" => Some("\x1b[C"),
        "left" => Some("\x1b[D"),
        "home" => Some("\x1b[H"),
        "end" => Some("\x1b[F"),
        "pageup" => Some("\x1b[5~"),
        "pagedown" => Some("\x1b[6~"),
        "ctrl+c" => Some("\x03"),
        "ctrl+d" => Some("\x04"),
        "ctrl+z" => Some("\x1a"),
        "ctrl+l" => Some("\x0c"),
        "ctrl+u" => Some("\x15"),
        "ctrl+a" => Some("\x01"),
        "ctrl+e" => Some("\x05"),
        "ctrl+\\" => Some("\x1c"),
        _ if normalized.chars().count() == 1 => Some(key),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct EncodedKeys {
    pub data: String,
    pub unknown: Vec<String>,
}

pub fn encode_keys(keys: &[String]) -> EncodedKeys {
    let mut result = EncodedKeys {
        data: String::new(),
        unknown: Vec::new(),
    };
    for key in keys {
        match resolve_key_sequence(key) {
            Some(sequence) => result.data.push_str(sequence),
            None => result.unknown.push(key.clone()),
        }
    }
    result
}

pub fn safe_reg_exp(pattern: &str) -> Option<fancy_regex::Regex> {
    fancy_regex::Regex::new(pattern).ok()
}

#[cfg(test)]
pub fn test_sh_options(command: &str) -> maho_pty::PtySessionOptions {
    crate::tools::spawn::command_options(command, None, None)
        .unwrap()
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_and_unicode_passthrough() {
        assert_eq!(resolve_key_sequence(" ENTER "), Some("\r"));
        assert_eq!(resolve_key_sequence("ctrl+c"), Some("\x03"));
        assert_eq!(resolve_key_sequence(" A "), Some(" A "));
        assert_eq!(resolve_key_sequence("\u{1f600}"), Some("\u{1f600}"));
        assert_eq!(resolve_key_sequence(""), None);
        assert_eq!(resolve_key_sequence("unknown"), None);
    }

    #[test]
    fn encoding_preserves_key_order_and_unknowns() {
        let keys = ["A", "enter", "bad-key", "ctrl+d"].map(str::to_owned);
        assert_eq!(
            encode_keys(&keys),
            EncodedKeys {
                data: "A\r\x04".to_owned(),
                unknown: vec!["bad-key".to_owned()]
            }
        );
    }

    #[test]
    fn runaway_summary_pluralizes_minutes_and_names_the_rearm_call() {
        assert_eq!(
            runaway_pause_summary("bash_1", 120, 5),
            "paused: runaway output (more than 120 matched lines a minute for 5 minutes); later lines stay in bash_output. Resume with monitor({ action: \"rearm\", bash_id: \"bash_1\" })."
        );
        assert!(runaway_pause_summary("b", 1, 1).contains("for 1 minute)"));
        assert!(runaway_pause_summary("b", 1, 2).starts_with(RUNAWAY_PAUSE_SUMMARY_PREFIX));
    }

    #[test]
    fn invalid_regex_is_absent() {
        assert!(safe_reg_exp("(").is_none());
        assert!(safe_reg_exp("^ready$").is_some());
    }
}
