use serde_json::{Value, json};

pub const BASH_DEFAULT_TIMEOUT_SECONDS: u64 = 1800;
pub const BASH_MAX_TIMEOUT_SECONDS: u64 = 1800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BashTimeoutDefaults {
    pub default_seconds: u64,
    pub max_seconds: u64,
}

fn positive_int(value: Option<&str>) -> Option<u64> {
    let value = value?.trim_start().strip_prefix('+').unwrap_or(value?.trim_start());
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u64>().ok().filter(|number| *number > 0)
}

pub fn resolve_bash_timeout_defaults(default: Option<&str>, maximum: Option<&str>) -> BashTimeoutDefaults {
    let default_seconds = positive_int(default).unwrap_or(BASH_DEFAULT_TIMEOUT_SECONDS);
    let max_seconds = positive_int(maximum).unwrap_or(BASH_MAX_TIMEOUT_SECONDS).max(default_seconds);
    BashTimeoutDefaults { default_seconds, max_seconds }
}

pub fn apply_bash_timeout(input: &Value, defaults: BashTimeoutDefaults) -> Value {
    let mut result = input.clone();
    if input.get("timeout").is_none() || input.get("timeout").and_then(Value::as_f64).is_some_and(|value| value <= 0.0) {
        result["timeout"] = json!(defaults.default_seconds);
    }
    result
}

pub fn build_bash_timeout_prompt(defaults: BashTimeoutDefaults, foreground_window_seconds: Option<f64>) -> String {
    let minutes = |seconds: u64| if seconds.is_multiple_of(60) { format!("{} min", seconds / 60) } else { format!("{seconds}s") };
    let detach = foreground_window_seconds.map_or_else(String::new, |seconds| format!("\n- Foreground blocking stops at the ~{seconds}s window. A command still running then auto-detaches alive to a background session with a `bash_id` and keeps running until it exits or hits the kill deadline; the session tools in the terminal section steer or stop it."));
    format!("\n## Bash Tool Timeout Policy\n\nThe `bash` tool's `timeout` parameter is the process kill deadline, not how long you wait for output: the command is killed when it reaches the deadline.\n\n- Default timeout: {}s ({}). Applied automatically when you do not set `timeout`.\n- Recommended maximum timeout: {}s ({}). Explicit `timeout` values are preserved because different hosts may use different timeout units.{detach}", defaults.default_seconds, minutes(defaults.default_seconds), defaults.max_seconds, minutes(defaults.max_seconds))
}
