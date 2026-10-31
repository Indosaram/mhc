use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
mod extension;
pub use extension::OrcaAgentStatus;

/// JS regex `\s`: ECMAScript WhiteSpace plus LineTerminator (U+FEFF yes, U+0085 no).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `contents.split(/\r?\n/)` matched against `/^(?:set\s+)?([A-Z0-9_]+)=(.*)$/`. Without the `s`
/// flag `.` stops at `\r`, U+2028 and U+2029, and `$` is end of input, so a line whose value still
/// carries one of them (e.g. a lone or doubled `\r`) does not match at all.
pub fn parse_endpoint(contents: &str) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    let segments = contents.split('\n').collect::<Vec<_>>();
    let last = segments.len() - 1;
    // `\r?\n` drops a `\r` only when a `\n` follows it, never on the final segment.
    for line in segments.iter().enumerate().map(|(index, line)| {
        if index < last {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        }
    }) {
        let line = if let Some(rest) = line.strip_prefix("set") {
            if rest.starts_with(is_js_space) {
                rest.trim_start_matches(is_js_space)
            } else {
                line
            }
        } else {
            line
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            continue;
        }
        if value.contains(['\r', '\u{2028}', '\u{2029}']) {
            continue;
        }
        output.insert(key.to_owned(), value.to_owned());
    }
    output
}

/// Env var the status owner publishes so inherited child processes stand down (upstream sets
/// `process.env.ORCA_PI_STATUS_OWNED = selfPid`).
pub const STATUS_OWNER_ENV: &str = "ORCA_PI_STATUS_OWNED";

/// Process-global owner pid. Mutating the process environment is `unsafe` in Rust, so the claim is
/// latched here and published through `maho_ext_api::process_env`, which every child spawner applies.
static STATUS_OWNER: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// Claim status reporting for `pid` unless the inherited `owner` names another process.
pub fn claim_status_owner(owner: Option<&str>, pid: u32) -> bool {
    if owner.is_some_and(|owner| !owner.is_empty() && owner != pid.to_string()) {
        return false;
    }
    let claimed = *STATUS_OWNER.get_or_init(|| pid) == pid;
    if claimed {
        maho_ext_api::process_env::set(STATUS_OWNER_ENV, &pid.to_string());
    }
    claimed
}

/// The pid that owns Orca status reporting in this process, for injection into child environments.
pub fn status_owner() -> Option<u32> {
    STATUS_OWNER.get().copied()
}

pub fn is_omp_runtime(names: &[&str]) -> bool {
    names.iter().any(|value| {
        let name = value
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        matches!(
            name.as_str(),
            "omp" | "omp.js" | "omp.sh" | "omp.cmd" | "omp.exe" | "omp.bat"
        )
    })
}

pub fn extract_assistant_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

pub struct HookEnvelope<'a> {
    pub pane_key: &'a str,
    pub launch_token: &'a str,
    pub tab_id: &'a str,
    pub worktree_id: &'a str,
    pub env: &'a str,
    pub version: &'a str,
}
pub fn status_payload(
    envelope: &HookEnvelope<'_>,
    event: &str,
    metadata: &Map<String, Value>,
    extra: &Map<String, Value>,
) -> Value {
    let mut payload = Map::new();
    payload.insert("hook_event_name".to_owned(), json!(event));
    payload.extend(metadata.clone());
    payload.extend(extra.clone());
    json!({"paneKey":envelope.pane_key,"launchToken":envelope.launch_token,"tabId":envelope.tab_id,"worktreeId":envelope.worktree_id,"env":envelope.env,"version":envelope.version,"payload":payload})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsed_when_posix_endpoint() {
        assert_eq!(
            parse_endpoint("PORT=123\r\nENV=local")
                .get("PORT")
                .map(String::as_str),
            Some("123")
        );
    }
    #[test]
    fn parsed_when_windows_endpoint() {
        assert_eq!(
            parse_endpoint("set PORT=123")
                .get("PORT")
                .map(String::as_str),
            Some("123")
        );
    }
    #[test]
    fn ignored_when_invalid_key() {
        assert!(parse_endpoint("lower=x\n KEY=y\nsetBAD=z").is_empty());
    }
    #[test]
    fn line_terminator_in_value_rejects_line() {
        let parsed = parse_endpoint("PORT=1\r\r\nENV=a\u{2028}b\nVERSION=2\rx\nTOKEN=t");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("TOKEN").map(String::as_str), Some("t"));
        assert!(parse_endpoint("PORT=1\r").is_empty());
    }
    #[test]
    fn set_prefix_uses_javascript_whitespace() {
        assert_eq!(
            parse_endpoint("set\u{feff}PORT=1")
                .get("PORT")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            parse_endpoint("set\u{a0}PORT=2")
                .get("PORT")
                .map(String::as_str),
            Some("2")
        );
        assert!(parse_endpoint("set\u{85}PORT=3").is_empty());
        assert_eq!(
            parse_endpoint("A=x\u{85}y").get("A").map(String::as_str),
            Some("x\u{85}y")
        );
    }
    #[test]
    fn last_when_duplicate_key() {
        assert_eq!(
            parse_endpoint("ENV=a\nENV=b")
                .get("ENV")
                .map(String::as_str),
            Some("b")
        );
    }
    #[test]
    fn omp_when_windows_executable() {
        assert!(is_omp_runtime(&["C:\\bin\\OMP.EXE"]));
    }
    #[test]
    fn pi_when_other_executable() {
        assert!(!is_omp_runtime(&["/bin/senpi"]));
    }
    #[test]
    fn status_owner_claim_rules() {
        assert!(!claim_status_owner(Some("1"), 2));
        assert!(claim_status_owner(Some(""), 7));
        assert!(claim_status_owner(Some("7"), 7));
        assert_eq!(status_owner(), Some(7));
        assert_eq!(
            maho_ext_api::process_env::vars()
                .get(STATUS_OWNER_ENV)
                .map(String::as_str),
            Some("7")
        );
    }
    #[test]
    fn text_when_content_parts() {
        assert_eq!(
            extract_assistant_text(
                &json!({"content":[{"type":"text","text":"a"},{"type":"thinking","text":"hidden"},{"type":"text","text":"b"}]})
            ),
            "ab"
        );
    }
    #[test]
    fn extra_when_payload_key_overridden() {
        let envelope = HookEnvelope {
            pane_key: "pane",
            launch_token: "",
            tab_id: "",
            worktree_id: "",
            env: "",
            version: "",
        };
        let extra = json!({"hook_event_name":"override"});
        let payload = status_payload(&envelope, "start", &Map::new(), extra.as_object().unwrap());
        assert_eq!(payload["payload"]["hook_event_name"], "override");
    }
}
