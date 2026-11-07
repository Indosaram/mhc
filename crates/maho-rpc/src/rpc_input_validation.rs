//! Inbound bounds and session-entry shape validation.
use serde_json::Value;
pub const MAX_RPC_MESSAGE_CHARACTERS: usize = 1_000_000;
pub const SESSION_CONTEXT_KEYS: usize = 32;
pub const SESSION_CONTEXT_VALUE_BYTES: usize = 16 * 1024;
pub const SESSION_CONTEXT_TOTAL_BYTES: usize = 32 * 1024;
/// Grouped caps for a session `context` map (rpc-input-validation.ts `SESSION_CONTEXT_LIMITS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionContextLimits {
    pub keys: usize,
    pub value_bytes: usize,
    pub total_bytes: usize,
}
pub const SESSION_CONTEXT_LIMITS: SessionContextLimits = SessionContextLimits {
    keys: SESSION_CONTEXT_KEYS,
    value_bytes: SESSION_CONTEXT_VALUE_BYTES,
    total_bytes: SESSION_CONTEXT_TOTAL_BYTES,
};
/// Session-role policy keys a host reads from the session `context` (capabilities
/// `memory_write_deny`, `effect_tools_deny`, `tools_exclude`, `supervisor_work_tools`).
pub const MEMORY_WRITE_CONTEXT_KEY: &str = "memory_write";
pub const MEMORY_WRITE_DENY: &str = "deny";
pub const EFFECT_TOOLS_CONTEXT_KEY: &str = "effect_tools";
pub const EFFECT_TOOLS_DENY: &str = "deny";
pub const TOOLS_EXCLUDE_CONTEXT_KEY: &str = "tools_exclude";
pub const SUPERVISOR_TOOLS_CONTEXT_KEY: &str = "supervisor_tools";
pub const SUPERVISOR_TOOLS_WORK: &str = "work";
pub const TOOLS_EXCLUDE_MAX_NAMES: usize = 128;
pub const TOOL_NAME_MAX_CHARS: usize = 64;

fn string(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(Value::is_string)
}
fn valid_content(content: &Value) -> bool {
    content.is_string()
        || content.as_array().is_some_and(|items| {
            items
                .iter()
                .all(|item| match item.get("type").and_then(Value::as_str) {
                    Some("text") => string(item, "text"),
                    Some("image") => string(item, "data") && string(item, "mimeType"),
                    _ => false,
                })
        })
}
fn valid_entry(entry: &Value) -> bool {
    if !entry.is_object()
        || !string(entry, "id")
        || !string(entry, "timestamp")
        || !entry
            .get("parentId")
            .is_some_and(|x| x.is_null() || x.is_string())
    {
        return false;
    }
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => entry.get("message").is_some_and(|m| {
            matches!(
                m.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "tool")
            ) && m.get("content").is_some_and(valid_content)
        }),
        Some("thinking_level_change") => string(entry, "thinkingLevel"),
        Some("model_change") => string(entry, "provider") && string(entry, "modelId"),
        Some("model_change_rejected") => {
            string(entry, "provider") && string(entry, "modelId") && string(entry, "detail")
        }
        Some("compaction") => {
            string(entry, "summary")
                && string(entry, "firstKeptEntryId")
                && entry.get("tokensBefore").is_some_and(Value::is_number)
        }
        Some("branch_summary") => string(entry, "fromId") && string(entry, "summary"),
        Some("custom") => string(entry, "customType"),
        Some("custom_message") => {
            string(entry, "customType")
                && entry.get("content").is_some_and(valid_content)
                && entry.get("display").is_some_and(Value::is_boolean)
        }
        Some("label") => {
            string(entry, "targetId") && entry.get("label").is_none_or(Value::is_string)
        }
        Some("session_info") => entry.get("name").is_none_or(Value::is_string),
        _ => false,
    }
}
pub fn rpc_command_shape_error(command: &Value) -> Option<&'static str> {
    (!command.is_object()).then_some("RPC command must be a JSON object.")
}
pub fn rpc_command_payload_error(command: &Value) -> Option<&'static str> {
    if let Some(error) = crate::pause_command_validation::validate_pause_command(command) {
        return Some(error);
    }
    match command.get("type").and_then(Value::as_str) {
        Some("edit_user_message")
            if command
                .get("entryId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
                || !string(command, "text") =>
        {
            Some("edit_user_message requires a non-empty entryId and a text string")
        }
        Some("edit_assistant_message")
            if command
                .get("entryId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
                || !string(command, "text") =>
        {
            Some("edit_assistant_message requires a non-empty entryId and a text string")
        }
        Some("set_fast_mode") if !command.get("enabled").is_some_and(Value::is_boolean) => {
            Some("set_fast_mode requires a boolean 'enabled' field.")
        }
        Some("append_user_message") if !command.get("content").is_some_and(valid_content) => {
            Some("append_user_message content must be a string or text/image content array.")
        }
        Some("append_session_entry") if !command.get("entry").is_some_and(valid_entry) => {
            Some("append_session_entry entry is malformed.")
        }
        Some("unload_session") => unload_session_error(command),
        Some("dequeue_message" | "edit_queued_message") => queued_input_change_error(command),
        Some("get_effect_status") => effect_status_error(command),
        Some("get_pending_inputs") => pending_inputs_error(command),
        _ => None,
    }
}
/// A non-empty string of at most 256 UTF-16 code units (senpi's `value.length <= 256`).
fn id_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty() && text.encode_utf16().count() <= 256)
}
/// senpi rpc-input-validation.ts `dequeue_message` / `edit_queued_message`.
fn queued_input_change_error(command: &Value) -> Option<&'static str> {
    if !id_string(command.get("queuedId")) {
        return Some(
            "queuedId must be a non-empty string of at most 256 characters; read it from get_state or queue_update.",
        );
    }
    if command["type"] == "edit_queued_message"
        && !command
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                !text.is_empty() && text.encode_utf16().count() <= MAX_RPC_MESSAGE_CHARACTERS
            })
    {
        return Some(
            "text must be a non-empty string of at most 1000000 characters; dequeue_message removes an input.",
        );
    }
    // `Number.isSafeInteger(n) && n >= 1`.
    if command.get("expectedRevision").is_some_and(|revision| {
        !revision
            .as_u64()
            .is_some_and(|revision| (1..=9_007_199_254_740_991).contains(&revision))
    }) {
        return Some(
            "expectedRevision must be a whole number of at least 1; read it from the input's revision in get_state or queue_update.",
        );
    }
    None
}
/// senpi rpc-input-validation.ts `get_effect_status`: a required `effectId`, an optional string `sessionPath`.
fn effect_status_error(command: &Value) -> Option<&'static str> {
    if !id_string(command.get("effectId")) {
        return Some("effectId must be a non-empty string of at most 256 characters.");
    }
    command
        .get("sessionPath")
        .is_some_and(|path| !path.is_string())
        .then_some("sessionPath must be a string.")
}
/// senpi rpc-input-validation.ts `get_pending_inputs`: at most 64 non-empty ids of at most 256 characters.
fn pending_inputs_error(command: &Value) -> Option<&'static str> {
    let ids = command.get("knownClientMessageIds")?;
    let valid = ids
        .as_array()
        .is_some_and(|ids| ids.len() <= 64 && ids.iter().all(|id| id_string(Some(id))));
    (!valid).then_some(
        "knownClientMessageIds must be an array of at most 64 non-empty strings of at most 256 characters.",
    )
}
/// senpi rpc-input-validation.ts `unload_session`: `children` is `refuse`/`detach`, the flags are booleans.
fn unload_session_error(command: &Value) -> Option<&'static str> {
    if command
        .get("children")
        .is_some_and(|children| children != "refuse" && children != "detach")
    {
        return Some("children must be \"refuse\" or \"detach\".");
    }
    [
        ("interrupt", "interrupt must be a boolean."),
        ("clearQueue", "clearQueue must be a boolean."),
        ("force", "force must be a boolean."),
    ]
    .into_iter()
    .find(|(flag, _)| command.get(*flag).is_some_and(|value| !value.is_boolean()))
    .map(|(_, error)| error)
}
pub fn rpc_message_length_error(command: &Value) -> Option<String> {
    let kind = command.get("type")?.as_str()?;
    if !matches!(kind, "prompt" | "steer" | "follow_up") {
        return None;
    }
    let message = command.get("message")?.as_str()?;
    (message.encode_utf16().count() > MAX_RPC_MESSAGE_CHARACTERS)
        .then(|| format!("RPC {kind} message exceeds {MAX_RPC_MESSAGE_CHARACTERS} characters."))
}
pub fn session_context_error(context: Option<&Value>) -> Option<String> {
    let context = context?;
    let Some(entries) = context.as_object() else {
        return Some("context must be an object of string values.".into());
    };
    if entries.len() > SESSION_CONTEXT_KEYS {
        return Some(format!(
            "context has {} keys, at most {SESSION_CONTEXT_KEYS} are accepted.",
            entries.len()
        ));
    }
    for (key, value) in entries {
        let mut chars = key.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
            || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Some(format!(
                "context key \"{key}\" must match ^[a-z][a-z0-9_]*$."
            ));
        }
        let Some(text) = value.as_str() else {
            return Some(format!("context value for key \"{key}\" must be a string."));
        };
        if text.len() > SESSION_CONTEXT_VALUE_BYTES {
            return Some(format!(
                "context value for key \"{key}\" is {} bytes, at most {SESSION_CONTEXT_VALUE_BYTES} bytes are accepted.",
                text.len()
            ));
        }
    }
    let total = context.to_string().len();
    if total > SESSION_CONTEXT_TOTAL_BYTES {
        return Some(format!(
            "context is {total} bytes of JSON, at most {SESSION_CONTEXT_TOTAL_BYTES} bytes are accepted."
        ));
    }
    let strings: std::collections::BTreeMap<String, String> = entries
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
        .collect();
    session_policy_error(Some(&strings))
}
/// Tool names of a `tools_exclude` value: comma-separated, trimmed, deduplicated in order.
pub fn parse_tools_exclude(value: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::<String>::new();
    for raw in value.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            return Err(format!(
                "{TOOLS_EXCLUDE_CONTEXT_KEY} must be a comma-separated list of tool names without empty items."
            ));
        }
        if name.chars().count() > TOOL_NAME_MAX_CHARS
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(format!(
                "{TOOLS_EXCLUDE_CONTEXT_KEY} item \"{name}\" is not a tool name (^[A-Za-z0-9_.-]{{1,{TOOL_NAME_MAX_CHARS}}}$)."
            ));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    if names.len() > TOOLS_EXCLUDE_MAX_NAMES {
        return Err(format!(
            "{TOOLS_EXCLUDE_CONTEXT_KEY} names {} tools, at most {TOOLS_EXCLUDE_MAX_NAMES} are accepted.",
            names.len()
        ));
    }
    Ok(names)
}
/// Shape errors of the session-role policy keys (`invalid_session_context`). Unknown tool names
/// need the mounted runtime, so the host checks them when it builds the session.
pub fn session_policy_error(
    context: Option<&std::collections::BTreeMap<String, String>>,
) -> Option<String> {
    let context = context?;
    if let Some(value) = context
        .get(MEMORY_WRITE_CONTEXT_KEY)
        .filter(|value| value.as_str() != MEMORY_WRITE_DENY)
    {
        return Some(format!(
            "context {MEMORY_WRITE_CONTEXT_KEY} must be \"{MEMORY_WRITE_DENY}\", got \"{value}\"."
        ));
    }
    if let Some(value) = context
        .get(EFFECT_TOOLS_CONTEXT_KEY)
        .filter(|value| value.as_str() != EFFECT_TOOLS_DENY)
    {
        return Some(format!(
            "context {EFFECT_TOOLS_CONTEXT_KEY} must be \"{EFFECT_TOOLS_DENY}\", got \"{value}\"."
        ));
    }
    if let Some(value) = context
        .get(SUPERVISOR_TOOLS_CONTEXT_KEY)
        .filter(|value| value.as_str() != SUPERVISOR_TOOLS_WORK)
    {
        return Some(format!(
            "context {SUPERVISOR_TOOLS_CONTEXT_KEY} must be \"{SUPERVISOR_TOOLS_WORK}\", got \"{value}\"."
        ));
    }
    context
        .get(TOOLS_EXCLUDE_CONTEXT_KEY)
        .and_then(|value| parse_tools_exclude(value).err())
}
pub fn session_kind_error(kind: Option<&Value>) -> Option<&'static str> {
    kind.filter(|v| !matches!(v.as_str(), Some("interactive" | "worker")))
        .map(|_| "kind must be \"interactive\" or \"worker\".")
}
/// The machine `errorCode` of an `open_session` failure. The host formats policy failures as
/// `invalid_session_context: ...`, possibly wrapped by the registry as `open_failed: <reason>`; only
/// that leading reason code is read, never free text further in.
pub fn open_session_error_code(message: &str) -> Option<&'static str> {
    let reason = message.strip_prefix("open_failed: ").unwrap_or(message);
    reason
        .strip_prefix(crate::rpc_types::RPC_ERROR_INVALID_SESSION_CONTEXT)
        .is_some_and(|rest| rest.starts_with(':'))
        .then_some(crate::rpc_types::RPC_ERROR_INVALID_SESSION_CONTEXT)
}
pub fn session_auto_title_error(value: Option<&Value>) -> Option<&'static str> {
    value
        .filter(|v| !v.is_boolean())
        .map(|_| "auto_title must be a boolean.")
}
pub fn expected_host_instance_error(value: Option<&Value>) -> Option<&'static str> {
    match value {
        None => None,
        Some(Value::String(text)) if !text.trim().is_empty() => None,
        Some(_) => Some("expectedHostInstance must be a non-empty string"),
    }
}

const MAX_RETRY_FALLBACK_CHAINS: usize = 32;
const MAX_RETRY_FALLBACK_ENTRIES: usize = 32;
const MAX_RETRY_FALLBACK_SELECTOR_LENGTH: usize = 512;

/// `sessionPromptSurfaceError`.
pub fn prompt_surface_error(value: Option<&Value>) -> Option<&'static str> {
    match value {
        None => None,
        Some(Value::String(surface)) if matches!(surface.as_str(), "terminal" | "app" | "chat") => {
            None
        }
        Some(_) => Some("promptSurface must be \"terminal\", \"app\" or \"chat\"."),
    }
}

/// `sessionBrowserEngineError`: an unknown value is never read as `none`.
pub fn browser_engine_error(value: Option<&Value>) -> Option<&'static str> {
    match value {
        None => None,
        Some(Value::String(engine))
            if matches!(engine.as_str(), "connected" | "builtin" | "none") =>
        {
            None
        }
        Some(_) => Some("browserEngine must be \"connected\", \"builtin\" or \"none\"."),
    }
}

fn is_selector(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.trim().is_empty() && text.encode_utf16().count() <= MAX_RETRY_FALLBACK_SELECTOR_LENGTH
    })
}

/// `sessionRetryFallbackError`: `{ modelFallback: boolean, fallbackChains: Record<string, string[]> }`, at most
/// 32 chains of at most 32 non-empty selectors.
pub fn retry_fallback_error(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let shape = "retryFallback must be { modelFallback: boolean, fallbackChains: Record<string, string[]> }.".to_owned();
    let Some(record) = value.as_object() else {
        return Some(shape);
    };
    if !record.get("modelFallback").is_some_and(Value::is_boolean) {
        return Some(shape);
    }
    let Some(chains) = record.get("fallbackChains").and_then(Value::as_object) else {
        return Some(shape);
    };
    if chains.len() > MAX_RETRY_FALLBACK_CHAINS {
        return Some(format!(
            "retryFallback allows at most {MAX_RETRY_FALLBACK_CHAINS} chains."
        ));
    }
    for (key, selectors) in chains {
        if !is_selector(&Value::String(key.clone())) {
            return Some(shape);
        }
        let Some(selectors) = selectors.as_array() else {
            return Some(shape);
        };
        if !selectors.iter().all(is_selector) {
            return Some(shape);
        }
        if selectors.len() > MAX_RETRY_FALLBACK_ENTRIES {
            return Some(format!(
                "retryFallback allows at most {MAX_RETRY_FALLBACK_ENTRIES} entries per chain."
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn context_limits_struct_matches_the_constants() {
        assert_eq!(
            SESSION_CONTEXT_LIMITS,
            SessionContextLimits {
                keys: 32,
                value_bytes: 16 * 1024,
                total_bytes: 32 * 1024
            }
        );
        assert_eq!(SESSION_CONTEXT_LIMITS.keys, SESSION_CONTEXT_KEYS);
    }
    // senpi rpc-input-validation.ts `unload_session`.
    #[test]
    fn unload_session_flags_are_validated() {
        let check = |command: Value| rpc_command_payload_error(&command);
        assert_eq!(
            check(json!({"type":"unload_session","children":"detach","force":true})),
            None
        );
        assert_eq!(
            check(json!({"type":"unload_session","children":"later"})),
            Some("children must be \"refuse\" or \"detach\".")
        );
        assert_eq!(
            check(json!({"type":"unload_session","clearQueue":"yes"})),
            Some("clearQueue must be a boolean.")
        );
    }
    // senpi rpc-input-validation.ts: queuedId, text, expectedRevision, effectId, sessionPath and
    // knownClientMessageIds are refused before anything is read (rpc-queued-input-edit, rpc-pending-inputs tests).
    #[test]
    fn queued_input_effect_and_pending_input_fields_are_validated() {
        let check = |command: Value| rpc_command_payload_error(&command);
        assert_eq!(
            check(
                json!({"type":"dequeue_message","queuedId":"q_1","expectedRevision":1,"effectId":"d1"})
            ),
            None
        );
        assert!(
            check(json!({"type":"dequeue_message","queuedId":""}))
                .is_some_and(|error| error.starts_with("queuedId must be"))
        );
        assert!(check(json!({"type":"dequeue_message"})).is_some());
        assert!(
            check(json!({"type":"dequeue_message","queuedId":"q_1","expectedRevision":0}))
                .is_some_and(|error| error.starts_with("expectedRevision must be"))
        );
        assert!(
            check(json!({"type":"dequeue_message","queuedId":"q_1","expectedRevision":1.5}))
                .is_some()
        );
        assert_eq!(
            check(json!({"type":"edit_queued_message","queuedId":"q_1","text":"fixed"})),
            None
        );
        assert!(
            check(json!({"type":"edit_queued_message","queuedId":"q_1","text":""}))
                .is_some_and(|error| error.starts_with("text must be"))
        );
        assert_eq!(
            check(json!({"type":"get_effect_status","effectId":"e1"})),
            None
        );
        assert!(check(json!({"type":"get_effect_status"})).is_some());
        assert_eq!(
            check(json!({"type":"get_effect_status","effectId":"e1","sessionPath":7})),
            Some("sessionPath must be a string.")
        );
        let ids = |count: usize| {
            (0..count)
                .map(|index| format!("c-{index}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            check(json!({"type":"get_pending_inputs","knownClientMessageIds":ids(64)})),
            None
        );
        assert_eq!(check(json!({"type":"get_pending_inputs"})), None);
        assert!(
            check(json!({"type":"get_pending_inputs","knownClientMessageIds":ids(65)}))
                .is_some_and(|error| error.contains("at most 64"))
        );
        assert!(check(json!({"type":"get_pending_inputs","knownClientMessageIds":[""]})).is_some());
    }
    #[test]
    fn rejects_nonobject_input() {
        for input in [Value::Null, json!([]), json!(42), json!("str"), json!(true)] {
            assert!(rpc_command_shape_error(&input).is_some());
        }
    }
    #[test]
    fn accepts_object_input() {
        assert_eq!(
            rpc_command_shape_error(&json!({"type":"get_commands"})),
            None
        );
    }
    #[test]
    fn rejects_oversized_prompt_text() {
        for kind in ["prompt", "steer", "follow_up"] {
            assert_eq!(
                rpc_message_length_error(
                    &json!({"type":kind,"message":"x".repeat(MAX_RPC_MESSAGE_CHARACTERS+1)})
                ),
                Some(format!(
                    "RPC {kind} message exceeds {MAX_RPC_MESSAGE_CHARACTERS} characters."
                ))
            );
        }
    }
    #[test]
    fn accepts_refused_model_entry_only_with_detail() {
        let mut entry = json!({"id":"entry-1","parentId":null,"timestamp":"fixed","type":"model_change_rejected","provider":"faux","modelId":"too-small","detail":"budget"});
        assert_eq!(
            rpc_command_payload_error(&json!({"type":"append_session_entry","entry":entry})),
            None
        );
        entry.as_object_mut().unwrap().remove("detail");
        assert!(
            rpc_command_payload_error(&json!({"type":"append_session_entry","entry":entry}))
                .is_some()
        );
    }
    #[test]
    fn accepts_maximum_and_ignores_nonmessage_command() {
        assert_eq!(
            rpc_message_length_error(
                &json!({"type":"prompt","message":"x".repeat(MAX_RPC_MESSAGE_CHARACTERS)})
            ),
            None
        );
        assert_eq!(
            rpc_message_length_error(&json!({"type":"get_commands"})),
            None
        );
    }
    #[test]
    fn context_counts_utf8_bytes() {
        assert!(
            session_context_error(Some(&json!({"label":"한".repeat(6000)})))
                .unwrap()
                .contains("18000 bytes")
        );
    }
    #[test]
    fn context_preserves_absent_versus_null() {
        assert_eq!(session_context_error(None), None);
        assert!(session_context_error(Some(&Value::Null)).is_some());
    }
    #[test]
    fn context_rejects_invalid_keys() {
        assert!(session_context_error(Some(&json!({"Bad":"ok"}))).is_some());
        assert_eq!(session_context_error(Some(&json!({"good_2":"ok"}))), None);
    }
    #[test]
    fn kind_rejects_unknown_and_null() {
        assert!(session_kind_error(Some(&json!("typo"))).is_some());
        assert!(session_kind_error(Some(&Value::Null)).is_some());
        assert_eq!(session_kind_error(None), None);
    }
    #[test]
    fn title_preserves_false_and_omitted() {
        assert_eq!(session_auto_title_error(Some(&json!(false))), None);
        assert_eq!(session_auto_title_error(None), None);
        assert!(session_auto_title_error(Some(&json!(0))).is_some());
    }
    #[test]
    fn expected_host_instance_validation_matches_upstream() {
        assert_eq!(expected_host_instance_error(None), None);
        assert_eq!(
            expected_host_instance_error(Some(&json!("host-inst-1"))),
            None
        );
        assert_eq!(
            expected_host_instance_error(Some(&json!(""))),
            Some("expectedHostInstance must be a non-empty string")
        );
        assert_eq!(
            expected_host_instance_error(Some(&json!("   "))),
            Some("expectedHostInstance must be a non-empty string")
        );
        assert_eq!(
            expected_host_instance_error(Some(&json!(123))),
            Some("expectedHostInstance must be a non-empty string")
        );
        assert_eq!(
            expected_host_instance_error(Some(&json!(true))),
            Some("expectedHostInstance must be a non-empty string")
        );
        assert_eq!(
            expected_host_instance_error(Some(&Value::Null)),
            Some("expectedHostInstance must be a non-empty string")
        );
    }
    fn policy(pairs: &[(&str, &str)]) -> Option<String> {
        let context: std::collections::BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        session_policy_error(Some(&context))
    }
    #[test]
    fn policy_accepts_only_the_documented_values() {
        assert_eq!(
            policy(&[
                ("memory_write", "deny"),
                ("supervisor_tools", "work"),
                ("tools_exclude", "bash, mcp_omo-memory_memory,edit")
            ]),
            None
        );
        for (key, value) in [
            ("memory_write", "allow"),
            ("memory_write", "Deny"),
            ("memory_write", ""),
            ("supervisor_tools", "admin"),
            ("tools_exclude", ""),
            ("tools_exclude", "bash,,edit"),
            ("tools_exclude", "rm -rf"),
            ("tools_exclude", "a/b"),
        ] {
            assert!(policy(&[(key, value)]).is_some(), "{key}={value}");
        }
        assert_eq!(session_policy_error(None), None);
        assert!(session_context_error(Some(&json!({"memory_write":"maybe"}))).is_some());
    }
    #[test]
    fn tools_exclude_trims_and_deduplicates() {
        assert_eq!(
            parse_tools_exclude(" bash ,edit,bash").unwrap(),
            vec!["bash", "edit"]
        );
        assert!(
            parse_tools_exclude(
                &(0..=TOOLS_EXCLUDE_MAX_NAMES)
                    .map(|index| format!("t{index}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .is_err()
        );
    }
    #[test]
    fn effect_tools_accepts_only_deny() {
        assert_eq!(
            policy(&[
                ("effect_tools", "deny"),
                ("tools_exclude", "bash,edit,write")
            ]),
            None
        );
        for value in ["allow", "Deny", "", " deny", "true"] {
            let error = policy(&[("effect_tools", value)])
                .unwrap_or_else(|| panic!("effect_tools={value:?} must be rejected"));
            assert!(error.contains("effect_tools"), "{error}");
        }
        assert!(session_context_error(Some(&json!({"effect_tools":"allow"}))).is_some());
    }
    #[test]
    fn open_failures_carry_the_typed_policy_code() {
        assert_eq!(
            open_session_error_code(
                "invalid_session_context: context memory_write must be \"deny\""
            ),
            Some("invalid_session_context")
        );
        assert_eq!(
            open_session_error_code(
                "open_failed: invalid_session_context: tools_exclude names no registered tool: x"
            ),
            Some("invalid_session_context")
        );
        assert_eq!(open_session_error_code("open_failed: spawn failed"), None);
        assert_eq!(
            open_session_error_code("open_failed: model said invalid_session_context: no"),
            None,
            "only the leading reason code types the failure"
        );
    }
}
