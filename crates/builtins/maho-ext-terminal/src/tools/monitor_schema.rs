//! Port of senpi `terminal/tools/monitor-schema.ts`: the flat monitor tool schema
//! (`monitorSchema`), the branch predicates and the value resolvers.
use serde_json::{Value, json};

pub use super::monitor::{DEFAULT_MONITOR_TIMEOUT_MS, MAX_MONITOR_TIMEOUT_MS};

/// `monitorSchema`: one flat object schema, no top-level union. Several provider payload paths
/// rebuild tool schemas from top-level `properties` only, so a root `anyOf` would reach the model
/// as an empty schema; branch requirements are enforced at runtime in `execute`.
pub fn monitor_schema() -> Value {
    json!({"type":"object","properties":{
    "action":{"type":"string","enum":["create","rearm"],"description":"Defaults to create. rearm resumes a monitor paused by the wake budget or the runaway guard."},
    "type":{"type":"string","enum":["command","path","rpc"],"description":"Create: which source to watch; inferred when omitted (command -> command, path -> path, target -> rpc)."},
    "target":{"type":"string","minLength":1,"description":"Create, rpc branch: \"children\" (sessions this one spawned, including ones spawned later), \"all\" (every session on every host and terminal of this user) or one session id, path or st_ task id."},
    "events":{"anyOf":[{"type":"string","const":"all"},{"type":"array","items":{"type":"string","minLength":1}}],"description":"Rpc branch: record kinds to report, any RPC record type; default [\"session_status\"] (children: plus \"effect_status\"); \"all\" reports every kind, streaming collapsed to one line per 2 s."},
    "exclude":{"type":"array","items":{"type":"string","minLength":1},"description":"Rpc branch with events \"all\": record kinds to drop."},
    "description":{"type":"string","minLength":1,"maxLength":200,"description":"Create (required): specific label shown with every event, e.g. 'errors in deploy.log'."},
    "command":{"type":"string","description":"Create, command branch (XOR path): shell command to run and watch in a PTY-backed monitor session."},
    "path":{"type":"string","minLength":1,"description":"Create, file branch (XOR command): one regular file to watch natively, whose parent directory must already exist; takes no filter."},
    "event":{"type":"string","enum":["create","modify"],"description":"File branch only: which file event fires the watch (defaults to create)."},
    "filter":{"type":"string","description":"Only lines matching this regex become monitor events (command output lines, rpc event lines)."},
    "timeout_ms":{"type":"number","minimum":1,"maximum":MAX_MONITOR_TIMEOUT_MS,"description":"Watcher deadline in milliseconds (default 300000; ignored when persistent)."},
    "persistent":{"type":"boolean","description":"Standing watch: no deadline, and it survives a session restart (command re-run once, file rescanned and any detached change reported). Expires 7 days after creation; no per-session cap unless the terminal.maxDurableMonitors setting sets one; stop one with kill_bash."},
    "bash_id":{"type":"string","description":"Rearm: paused monitor id (mon_ or bash_id) to resume; omit for all paused."},
    "wakes_per_hour":{"type":"integer","minimum":1,"maximum":3600,"description":"Persistent command branch only: average wakes an hour for this watch (default: terminal.monitorWakesPerHour, 12). Lines matched between wakes are batched into the next one, never dropped; raise it for a chat bridge that needs quicker replies."},
    "human_input":{"type":"boolean","description":"Command branch only: set true when each output line is a message a person wrote (a chat or inbox bridge). Its deliveries then count as new user input, so a running goal keeps going; leave unset for machine status."}}})
}

fn non_empty_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// `isFileCreateInput`: a description and a path.
pub fn is_file_create_input(input: &Value) -> bool {
    non_empty_str(input, "description").is_some() && non_empty_str(input, "path").is_some()
}

/// `isCreateInput`: a description and a command.
pub fn is_create_input(input: &Value) -> bool {
    non_empty_str(input, "description").is_some() && non_empty_str(input, "command").is_some()
}

/// `resolveDimension`: a finite value of at least 1, truncated; otherwise the fallback.
pub fn resolve_dimension(value: Option<f64>, fallback: u16) -> u16 {
    match value {
        Some(value) if value.is_finite() && value >= 1.0 => {
            value.trunc().min(f64::from(u16::MAX)) as u16
        }
        _ => fallback,
    }
}

/// `resolveTimeoutMs`: default 300000, truncated and clamped to 1..=3600000.
pub fn resolve_timeout_ms(value: Option<f64>) -> u64 {
    let timeout = value.unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64);
    timeout.trunc().clamp(1.0, MAX_MONITOR_TIMEOUT_MS as f64) as u64
}

/// `compileFilter`: `None` without a filter; the JavaScript RegExp dialect maps to fancy-regex.
pub fn compile_filter(
    filter: Option<&str>,
) -> Result<Option<fancy_regex::Regex>, Box<fancy_regex::Error>> {
    filter
        .map(fancy_regex::Regex::new)
        .transpose()
        .map_err(Box::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `monitorSchema`: every upstream field, flat (no root union), with the upstream enums and bounds.
    #[test]
    fn schema_is_flat_and_carries_every_upstream_field() {
        let schema = monitor_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("anyOf").is_none() && schema.get("oneOf").is_none());
        let properties = schema["properties"].as_object().unwrap();
        let mut names = properties.keys().map(String::as_str).collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "action",
                "bash_id",
                "command",
                "description",
                "event",
                "events",
                "exclude",
                "filter",
                "human_input",
                "path",
                "persistent",
                "target",
                "timeout_ms",
                "type",
                "wakes_per_hour"
            ]
        );
        assert_eq!(properties["action"]["enum"], json!(["create", "rearm"]));
        assert_eq!(
            properties["type"]["enum"],
            json!(["command", "path", "rpc"])
        );
        assert_eq!(properties["event"]["enum"], json!(["create", "modify"]));
        assert_eq!(properties["target"]["minLength"], 1);
        assert_eq!(properties["events"]["anyOf"][0]["const"], "all");
        assert_eq!(properties["events"]["anyOf"][1]["items"]["minLength"], 1);
        assert_eq!(properties["description"]["maxLength"], 200);
        assert_eq!(properties["timeout_ms"]["maximum"], MAX_MONITOR_TIMEOUT_MS);
        assert_eq!(properties["wakes_per_hour"]["type"], "integer");
        assert_eq!(properties["wakes_per_hour"]["maximum"], 3600);
    }

    #[test]
    fn branch_predicates() {
        assert!(is_file_create_input(&json!({"description":"d","path":"p"})));
        assert!(!is_file_create_input(&json!({"description":"","path":"p"})));
        assert!(is_create_input(&json!({"description":"d","command":"c"})));
        assert!(!is_create_input(&json!({"description":"d","command":""})));
    }

    #[test]
    fn resolvers() {
        assert_eq!(resolve_dimension(None, 80), 80);
        assert_eq!(resolve_dimension(Some(0.5), 80), 80);
        assert_eq!(resolve_dimension(Some(f64::NAN), 80), 80);
        assert_eq!(resolve_dimension(Some(120.9), 80), 120);
        assert_eq!(resolve_timeout_ms(None), 300_000);
        assert_eq!(resolve_timeout_ms(Some(0.0)), 1);
        assert_eq!(resolve_timeout_ms(Some(9e9)), 3_600_000);
        assert_eq!(resolve_timeout_ms(Some(1500.7)), 1500);
        assert!(compile_filter(None).unwrap().is_none());
        assert!(
            compile_filter(Some("err(or)?"))
                .unwrap()
                .unwrap()
                .is_match("error")
                .unwrap()
        );
        assert!(compile_filter(Some("(")).is_err());
    }
}
