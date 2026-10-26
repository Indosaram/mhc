use crate::rpc_types::{
    ResponseRecordType, RpcCommand, RpcCommandBody, RpcResponse, RpcResponseResult,
};
use maho_core::agent_session::{AgentSession, QueuedInputOptions};
pub fn json_parse_error_message(input: &str) -> String {
    struct Parser<'a> {
        bytes: &'a [u8],
        at: usize,
    }
    impl Parser<'_> {
        fn space(&mut self) {
            while self
                .bytes
                .get(self.at)
                .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
            {
                self.at += 1;
            }
        }
        fn string(&mut self) -> Result<(), String> {
            self.at += 1;
            while let Some(&byte) = self.bytes.get(self.at) {
                self.at += 1;
                match byte {
                    b'"' => return Ok(()),
                    0..=31 => return Err("Unterminated string".into()),
                    b'\\' => {
                        let Some(&escape) = self.bytes.get(self.at) else {
                            return Err("Unterminated string".into());
                        };
                        self.at += 1;
                        match escape {
                            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                            b'u' => {
                                let digits =
                                    &self.bytes[self.at..self.bytes.len().min(self.at + 4)];
                                if digits.len() < 4 || digits.contains(&b'"') {
                                    return Err("\\u must be followed by 4 hex digits".into());
                                }
                                if !digits.iter().all(u8::is_ascii_hexdigit) {
                                    return Err(format!(
                                        "\"\\u{}\" is not a valid unicode escape",
                                        String::from_utf8_lossy(digits)
                                    ));
                                }
                                self.at += 4;
                            }
                            _ => {
                                return Err(format!(
                                    "Invalid escape character {}",
                                    String::from_utf8_lossy(&self.bytes[self.at - 1..])
                                        .chars()
                                        .next()
                                        .unwrap_or_default()
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Err("Unterminated string".into())
        }
        fn value(&mut self) -> Result<(), String> {
            self.space();
            let Some(&byte) = self.bytes.get(self.at) else {
                return Err("Unexpected EOF".into());
            };
            match byte {
                b'"' => self.string(),
                b'-' | b'0'..=b'9' => {
                    if byte == b'-' {
                        self.at += 1;
                    }
                    match self.bytes.get(self.at) {
                        Some(b'0') => self.at += 1,
                        Some(b'1'..=b'9') => {
                            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                                self.at += 1;
                            }
                        }
                        _ => return Err("Invalid number".into()),
                    }
                    if self.bytes.get(self.at) == Some(&b'.') {
                        self.at += 1;
                        if !self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                            return Err("Invalid digits after decimal point".into());
                        }
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                            self.at += 1;
                        }
                    }
                    if self
                        .bytes
                        .get(self.at)
                        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
                    {
                        self.at += 1;
                        if self
                            .bytes
                            .get(self.at)
                            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
                        {
                            self.at += 1;
                        }
                        if !self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                            return Err("Unable to parse JSON string".into());
                        }
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                            self.at += 1;
                        }
                    }
                    Ok(())
                }
                byte if byte.is_ascii_alphabetic() => {
                    for literal in [b"true".as_slice(), b"false", b"null"] {
                        if self.bytes[self.at..].starts_with(literal) {
                            self.at += literal.len();
                            return Ok(());
                        }
                    }
                    let start = self.at;
                    while self
                        .bytes
                        .get(self.at)
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        self.at += 1;
                    }
                    Err(format!(
                        "Unexpected identifier \"{}\"",
                        String::from_utf8_lossy(&self.bytes[start..self.at])
                    ))
                }
                b'}' | b']' | b',' | b':' => {
                    Err(format!("Unexpected token '{}'", char::from(byte)))
                }
                _ => Err(format!(
                    "Unrecognized token '{}'",
                    String::from_utf8_lossy(&self.bytes[self.at..])
                        .chars()
                        .next()
                        .unwrap_or_default()
                )),
            }
        }
        fn document(&mut self) -> Result<(), String> {
            let mut containers = Vec::new();
            loop {
                self.space();
                match self.bytes.get(self.at) {
                    Some(b'{') => {
                        self.at += 1;
                        self.space();
                        if self.bytes.get(self.at) == Some(&b'}') {
                            self.at += 1;
                        } else {
                            if self.bytes.get(self.at) != Some(&b'"') {
                                return Err("Expected '}'".into());
                            }
                            self.string()?;
                            self.space();
                            if self.bytes.get(self.at) != Some(&b':') {
                                return Err(
                                    "Expected ':' before value in object property definition"
                                        .into(),
                                );
                            }
                            self.at += 1;
                            containers.push(b'}');
                            continue;
                        }
                    }
                    Some(b'[') => {
                        self.at += 1;
                        self.space();
                        if self.bytes.get(self.at) == Some(&b']') {
                            self.at += 1;
                        } else {
                            containers.push(b']');
                            continue;
                        }
                    }
                    _ => self.value()?,
                }
                loop {
                    self.space();
                    let Some(&closing) = containers.last() else {
                        return if self.at == self.bytes.len() {
                            Ok(())
                        } else {
                            Err("Unable to parse JSON string".into())
                        };
                    };
                    if self.bytes.get(self.at) == Some(&closing) {
                        self.at += 1;
                        containers.pop();
                        continue;
                    }
                    if self.bytes.get(self.at) != Some(&b',') {
                        return Err(format!("Expected '{}'", char::from(closing)));
                    }
                    self.at += 1;
                    self.space();
                    if closing == b']' {
                        if self.bytes.get(self.at) == Some(&b']') {
                            return Err("Unexpected comma at the end of array expression".into());
                        }
                    } else {
                        if self.bytes.get(self.at) != Some(&b'"') {
                            return Err("Property name must be a string literal".into());
                        }
                        self.string()?;
                        self.space();
                        if self.bytes.get(self.at) != Some(&b':') {
                            return Err(
                                "Expected ':' before value in object property definition".into()
                            );
                        }
                        self.at += 1;
                    }
                    break;
                }
            }
        }
    }
    let mut parser = Parser {
        bytes: input.as_bytes(),
        at: 0,
    };
    let result = parser.document();
    format!(
        "JSON Parse error: {}",
        result
            .err()
            .unwrap_or_else(|| "Unable to parse JSON string".into())
    )
}
pub fn json_parse_error_code_units(input: &str) -> Vec<u16> {
    let message = json_parse_error_message(input);
    let mut units = message.encode_utf16().collect::<Vec<_>>();
    // JavaScriptCore names an unexpected token by its first UTF-16 code unit,
    // including a lone high surrogate for an astral character.
    if (message.starts_with("JSON Parse error: Unrecognized token '")
        || message.starts_with("JSON Parse error: Invalid escape character "))
        && let Some(index) = units
            .iter()
            .position(|unit| (0xd800..=0xdbff).contains(unit))
        && units
            .get(index + 1)
            .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
    {
        units.remove(index + 1);
    }
    units
}
pub fn json_parse_error_response(input: &str) -> String {
    use std::fmt::Write;
    let mut error = String::from("Failed to parse command: ")
        .encode_utf16()
        .collect::<Vec<_>>();
    error.extend(json_parse_error_code_units(input));
    let mut response =
        String::from("{\"type\":\"response\",\"command\":\"parse\",\"success\":false,\"error\":\"");
    for unit in error {
        let _ = write!(response, "\\u{unit:04x}");
    }
    response.push_str("\"}\n");
    response
}
pub type RpcRecordSink = std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>;
/// The abort owner the connection last observed on `agent_end`, keyed by the session id it was
/// seen on: `get_state` reports it as `lastAbortSource` until the session is replaced (senpi keeps
/// `lastAbortSource` per handler and clears it in `rebindSession`; native replacement keeps the
/// session object and changes its id).
pub type LastAbortSource = std::sync::Arc<std::sync::Mutex<Option<(String, &'static str)>>>;
/// senpi's subscribe-path enrichment inside `rebindSession`: `thinking_level_changed` and
/// `model_changed` carry the session's `thinkingSelection`, and `agent_end` records its abort owner.
pub fn rpc_session_event_record(
    session: &AgentSession,
    event: &maho_ext_api::AgentSessionEvent,
    last_abort_source: &LastAbortSource,
) -> Result<serde_json::Value, serde_json::Error> {
    let mut record = crate::session_binding::session_event_record(event)?;
    match event {
        maho_ext_api::AgentSessionEvent::ThinkingLevelChanged { .. }
        | maho_ext_api::AgentSessionEvent::ModelChanged { .. } => {
            if let Some(selection) = session.thinking_selection() {
                record["thinkingSelection"] = serde_json::to_value(selection)?;
            }
        }
        // senpi agent-session.ts publishes `message_update` through `withResolvedToolName`.
        maho_ext_api::AgentSessionEvent::Agent(
            event @ maho_ext_api::AgentEvent::MessageUpdate { .. },
        ) => {
            if let Some(name) = session.message_update_resolved_tool_name(event) {
                record["resolvedToolName"] = name.into();
            }
        }
        // senpi connection-handler.ts: `aborted` and `abortSource` are added together, only when the
        // session reports an abort owner for this turn.
        maho_ext_api::AgentSessionEvent::AgentEnd {
            abort_source: Some(source),
            ..
        } => {
            let source = match source {
                maho_ext_api::AbortSource::User => "user",
                maho_ext_api::AbortSource::System => "system",
                maho_ext_api::AbortSource::Provider => "provider",
            };
            *last_abort_source
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((session.session_id(), source));
            record["aborted"] = true.into();
            record["abortSource"] = source.into();
        }
        _ => {}
    }
    Ok(crate::session_record_stream::session_record(record))
}
fn current_abort_source(
    session: &AgentSession,
    last_abort_source: Option<&LastAbortSource>,
) -> Option<&'static str> {
    let stored = last_abort_source?
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    (stored.0 == session.session_id()).then_some(stored.1)
}
type ActiveLogins = std::sync::Arc<
    std::sync::Mutex<std::collections::HashMap<String, maho_ai::utils::abort::AbortController>>,
>;
pub async fn handle_input_line(
    session: &AgentSession,
    line: &str,
) -> Result<Option<String>, serde_json::Error> {
    handle_input_line_with_sink(session, line, None).await
}
pub async fn handle_input_line_with_sink(
    session: &AgentSession,
    line: &str,
    sink: Option<RpcRecordSink>,
) -> Result<Option<String>, serde_json::Error> {
    handle_input_line_with_login_state(session, line, sink, None, None, None).await
}
/// `handle_input_line_with_sink` for a connection that tracks `lastAbortSource` for `get_state`.
pub async fn handle_input_line_with_abort_source(
    session: &AgentSession,
    line: &str,
    sink: Option<RpcRecordSink>,
    last_abort_source: &LastAbortSource,
) -> Result<Option<String>, serde_json::Error> {
    handle_input_line_with_login_state(session, line, sink, None, None, Some(last_abort_source))
        .await
}
async fn handle_input_line_with_login_state(
    session: &AgentSession,
    line: &str,
    sink: Option<RpcRecordSink>,
    active_logins: Option<ActiveLogins>,
    ui: Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>,
    last_abort_source: Option<&LastAbortSource>,
) -> Result<Option<String>, serde_json::Error> {
    let parsed = serde_json::from_str::<serde_json::Value>(line);
    if parsed.is_err() {
        return Ok(Some(json_parse_error_response(line)));
    }
    let error = match &parsed {
        Err(_) => None,
        Ok(value) => crate::rpc_input_validation::rpc_command_shape_error(value).map(str::to_owned),
    };
    if let Some(error) = error {
        return crate::jsonl::serialize_json_line(&RpcResponse {
            id: None,
            record_type: ResponseRecordType::Response,
            command: "parse".into(),
            session_id: None,
            result: RpcResponseResult::Error {
                error,
                error_code: None,
                error_data: None,
            },
        })
        .map(Some);
    }
    let value = parsed?;
    let error = crate::rpc_input_validation::rpc_command_payload_error(&value)
        .map(str::to_owned)
        .or_else(|| crate::rpc_input_validation::rpc_message_length_error(&value));
    if let Some(error) = error {
        return crate::jsonl::serialize_json_line(&RpcResponse {
            id: value["id"].as_str().map(str::to_owned),
            record_type: ResponseRecordType::Response,
            command: value["type"].as_str().unwrap_or_default().into(),
            session_id: None,
            result: RpcResponseResult::Error {
                error,
                error_code: None,
                error_data: default_error_data(session, value["type"].as_str().unwrap_or_default()),
            },
        })
        .map(Some);
    }
    // PERF-DIVERGENCE(senpi): upstream passes the parsed JSON object straight on; we deserialize from `&Value` instead of deep-cloning the whole command (MBs with inline images). Same command and same serde error text.
    let command: RpcCommand = match <RpcCommand as serde::Deserialize>::deserialize(&value) {
        Ok(command) => command,
        Err(error)
            if error.to_string().starts_with(&format!(
                "unknown variant `{}`,",
                value["type"].as_str().unwrap_or_default()
            )) =>
        {
            let kind = value["type"].as_str().unwrap_or_default();
            return crate::jsonl::serialize_json_line(&RpcResponse {
                id: value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                record_type: ResponseRecordType::Response,
                command: kind.into(),
                session_id: value
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                result: RpcResponseResult::Error {
                    error: format!("Unknown command: {kind}"),
                    error_code: None,
                    error_data: None,
                },
            })
            .map(Some);
        }
        // Upstream answers any command failure with an error response (connection-handler.ts
        // `handleInputLine` catch); a malformed field must not end the command loop.
        Err(error) => {
            let kind = value["type"].as_str().unwrap_or_default();
            return crate::jsonl::serialize_json_line(&RpcResponse {
                id: value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                record_type: ResponseRecordType::Response,
                command: kind.into(),
                session_id: value
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                result: RpcResponseResult::Error {
                    error: error.to_string(),
                    error_code: None,
                    error_data: None,
                },
            })
            .map(Some);
        }
    };
    // Boxed: this command future is very large and would otherwise sit on the caller's stack.
    dispatch_session_command(session, &command, sink, active_logins, ui, last_abort_source)
    .await
    .map(|response| crate::jsonl::serialize_json_line(&response))
    .transpose()
}
/// The wire shape of one remembered model entry: the model plus its remembered thinking
/// level/selection and service tier (senpi's `RpcSessionModelEntry` spread).
pub fn session_model_entry_json(
    entry: &maho_core::agent_session::SessionModelEntry,
) -> serde_json::Value {
    let mut value = serde_json::json!({"model":&entry.model});
    if let Some(level) = &entry.thinking_level {
        value["thinkingLevel"] = serde_json::to_value(level).unwrap_or(serde_json::Value::Null);
    }
    if let Some(selection) = &entry.thinking_selection
        && let Ok(selection) = serde_json::to_value(selection)
    {
        value["thinkingSelection"] = selection;
    }
    if let Some(tier) = entry.service_tier {
        value["serviceTier"] = match tier {
            maho_ext_api::ServiceTier::Auto => "auto",
            maho_ext_api::ServiceTier::Flex => "flex",
            maho_ext_api::ServiceTier::Priority => "priority",
            maho_ext_api::ServiceTier::Ultrafast => "ultrafast",
        }
        .into();
    }
    value
}
/// Milliseconds since the epoch, the clock the RPC host stamps records with.
pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |elapsed| elapsed.as_secs_f64() * 1000.)
}
/// trust.json path, cwd and file stamp (mtime, len) of the last lookup, with its answer.
type TrustLookup = (String, String, Option<(std::time::SystemTime, u64)>, bool);
static LAST_TRUST_LOOKUP: std::sync::Mutex<Option<TrustLookup>> = std::sync::Mutex::new(None);
/// `ProjectTrustStore::get` behind a stat-keyed memo: snapshots are rebuilt on every session event,
/// and the store answer only changes when trust.json itself does.
fn project_trusted(agent_dir: &str, cwd: String) -> bool {
    let store = maho_core::ProjectTrustStore::new(agent_dir);
    let stamp = std::fs::metadata(store.trust_path())
        .ok()
        .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())));
    {
        let last = LAST_TRUST_LOOKUP
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((path, cached_cwd, cached_stamp, trusted)) = last.as_ref()
            && path == store.trust_path()
            && *cached_cwd == cwd
            && *cached_stamp == stamp
        {
            return *trusted;
        }
    }
    // The store lookup takes the trust.json file lock, so it runs with the memo mutex released.
    let decision = store.get(&cwd);
    let trusted = decision
        .as_ref()
        .is_ok_and(|decision| *decision == Some(true));
    // A failed lookup (lock timeout, bad file) is retried next time rather than memoized.
    *LAST_TRUST_LOOKUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = decision
        .is_ok()
        .then(|| (store.trust_path().to_owned(), cwd, stamp, trusted));
    trusted
}
/// Project one session into the wire state shape (senpi `buildRpcSessionState`).
///
/// Shared with `open_session` so both surfaces answer with the SAME fields, instead of a
/// second hand-rolled literal that silently drifts.
pub fn build_rpc_session_state(
    session: &AgentSession,
    last_abort_source: Option<&str>,
) -> serde_json::Value {
    build_rpc_session_state_with_dialogs(session, last_abort_source, Vec::new())
}
/// `build_rpc_session_state` plus the live extension dialog frames (senpi `pendingDialogs`:
/// `sessionDialogRequests.get(session)?.snapshot() ?? []`).
pub fn build_rpc_session_state_with_dialogs(
    session: &AgentSession,
    last_abort_source: Option<&str>,
    pending_dialogs: Vec<serde_json::Value>,
) -> serde_json::Value {
    // senpi's `buildRpcSessionState` consults the project-trust store directly (not the settings
    // manager's cached copy) and always publishes the session manager's accumulated usage totals.
    let project_trusted = project_trusted(&session.agent_dir(), session.cwd());
    let usage_totals = session.with_session_manager(|manager| manager.usage_totals().clone());
    let mut state = serde_json::json!({
        "model":session.model(),
        "thinkingLevel":session.thinking_level(),
        "serviceTier":session.effective_service_tier().map(|tier|match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority", maho_ext_api::ServiceTier::Ultrafast=>"ultrafast"}),
        "fastMode":session.is_fast_mode_active(),
        "isStreaming":session.is_streaming(),
        "isCompacting":session.is_compacting(),
        "retryAttempt":session.retry_attempt(),
        "isBashRunning":session.is_bash_running(),
        "paused":session.paused(),
        "runPaused":session.holds().iter().any(|hold|hold.kind==maho_core::engine_turn_hold::HoldKind::RunFence),
        "pause":{"pauseSeq":session.pause_seq(),"records":session.holds().iter().map(|hold|{let mut record=serde_json::json!({"kind":hold.kind.as_str(),"key":hold.key});if let Some(data)=&hold.data{for key in ["runRef","parentSessionId"]{if let Some(value)=data.get(key){record[key]=value.clone();}}}record}).collect::<Vec<_>>()},
        "pauseSeq":session.pause_seq(),
        "pausedHeldInputCount":session.paused_held_input_count(),
        "pausedNoticeCount":session.paused_notice_count(),
        "holds":session.holds().iter().map(|hold|serde_json::json!({"kind":hold.kind.as_str(),"key":hold.key,"since":hold.since,"data":hold.data,"sourceCounts":hold.source_counts})).collect::<Vec<_>>(),
        "pendingDialogs":pending_dialogs,
        "steeringMode":match session.steering_mode(){maho_agent::types::QueueMode::All=>"all",maho_agent::types::QueueMode::OneAtATime=>"one-at-a-time"},
        "followUpMode":match session.follow_up_mode(){maho_agent::types::QueueMode::All=>"all",maho_agent::types::QueueMode::OneAtATime=>"one-at-a-time"},
        "sessionFile":session.session_file(),
        "sessionId":session.session_id(),
        "sessionName":session.session_name(),
        "cwd":session.cwd(),
        "projectTrusted":project_trusted,
        "usageTotals":{"input":usage_totals.input,"output":usage_totals.output,"cacheRead":usage_totals.cache_read,"cacheWrite":usage_totals.cache_write,"cost":usage_totals.cost},
        "autoCompactionEnabled":session.auto_compaction_enabled(),
        "messageCount":session.message_count(),
        "pendingMessageCount":session.pending_message_count(),
        "steering":session.get_steering_messages(),
        "followUp":session.get_follow_up_messages(),
        "ordered":session.get_queued_input_order().iter().map(|input|serde_json::json!({"text":input.text,"mode":match input.mode{maho_ext_api::StreamingBehavior::Steer=>"steer",maho_ext_api::StreamingBehavior::FollowUp=>"followUp"},"enqueueOrder":input.enqueue_order})).collect::<Vec<_>>(),
        "favoriteModels":session.favorite_models().iter().map(session_model_entry_json).collect::<Vec<_>>(),
        "scopedModels":session.scoped_models().iter().map(session_model_entry_json).collect::<Vec<_>>(),
    });
    if let Some(rate) = usage_totals.latest_cache_hit_rate {
        state["usageTotals"]["latestCacheHitRate"] = rate.into();
    }
    if let Some(thinking_selection) = session.thinking_selection()
        && let Ok(value) = serde_json::to_value(thinking_selection)
    {
        state["thinkingSelection"] = value;
    }
    if let Some(abort_source) = last_abort_source {
        state["lastAbortSource"] = abort_source.into();
    }
    if let Some(context_usage) = session.get_context_usage() {
        state["contextUsage"] = serde_json::json!({"tokens":context_usage.tokens,"contextWindow":context_usage.context_window,"percent":context_usage.percent});
    }
    // Entries are published only while the session has no file on disk yet: a client that misses
    // them can never reconstruct the session it is bound to (senpi's existsSync guard).
    let session_file = session.session_file();
    if session_file
        .as_deref()
        .is_none_or(|file| !std::path::Path::new(file).exists())
    {
        // PERF-DIVERGENCE(senpi buildRpcSessionState): upstream copies the entries and re-serializes them on every event; we move the single `entries()` snapshot into the state instead of a second `json!(entries)` deep copy. Same JSON.
        let entries = session.with_session_manager(|manager| manager.entries());
        if entries.iter().any(|entry| {
            !matches!(
                entry["type"].as_str(),
                Some("model_change" | "model_change_rejected" | "thinking_level_change")
            )
        }) {
            state["entries"] = serde_json::Value::Array(entries);
        }
    }
    state
}
/// The leading capabilities of the classic `get_protocol_info` list that precede the negotiated ones.
const CLASSIC_BASE_CAPABILITIES: usize = 3;
pub async fn handle_session_command(
    session: &AgentSession,
    command: &RpcCommand,
) -> Option<RpcResponse> {
    dispatch_session_command(session, command, None, None, None, None).await
}
/// Prompts run in their own boxed future so the large generic command future is not polled beneath the agent run.
async fn dispatch_session_command(
    session: &AgentSession,
    command: &RpcCommand,
    sink: Option<RpcRecordSink>,
    active_logins: Option<ActiveLogins>,
    ui: Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>,
    last_abort_source: Option<&LastAbortSource>,
) -> Option<RpcResponse> {
    if matches!(command.body, RpcCommandBody::Prompt { .. }) {
        return Box::pin(run_prompt_command(session, command, sink)).await;
    }
    Box::pin(handle_session_command_with_sink(
        session,
        command,
        sink,
        active_logins,
        ui,
        last_abort_source,
    ))
    .await
}
/// senpi connection-handler.ts: a `MissingSessionCwdError` is answered with
/// `errorCode: "missing_session_cwd"` and the issue as `errorData`, so the client can offer to
/// continue in its current cwd.
fn session_switch_failure(
    command: &RpcCommand,
    kind: &str,
    error: maho_core::agent_session::SessionSwitchFailure,
) -> RpcResponse {
    let error_data=error.missing_cwd.map(|missing|{
        let mut data=serde_json::json!({"sessionCwd":missing.issue.session_cwd,"fallbackCwd":missing.issue.fallback_cwd});
        if let Some(file)=missing.issue.session_file{data["sessionFile"]=file.into();}
        data
    });
    RpcResponse {
        id: command.id.clone(),
        record_type: ResponseRecordType::Response,
        command: kind.into(),
        session_id: command.session_id.clone(),
        result: RpcResponseResult::Error {
            error: error.message,
            error_code: error_data
                .is_some()
                .then(|| crate::rpc_types::RPC_ERROR_MISSING_SESSION_CWD.to_owned()),
            error_data,
        },
    }
}
fn edit_response(
    session: &AgentSession,
    result: Result<maho_core::agent_session::AssistantEditResult, String>,
    not_persisted: &str,
) -> Result<Option<serde_json::Value>, String> {
    let result = result?;
    let leaf_id = session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
    if result.unchanged == Some(true) {
        return Ok(Some(
            serde_json::json!({"outcome":"unchanged","leafId":leaf_id}),
        ));
    }
    if result.cancelled {
        let mut value = serde_json::json!({"outcome":"cancelled","leafId":leaf_id});
        if result.aborted == Some(true) {
            value["aborted"] = true.into();
        }
        return Ok(Some(value));
    }
    let entry = result
        .entry_id
        .as_deref()
        .and_then(|id| session.with_session_manager(|manager| manager.entry(id)));
    if entry
        .as_ref()
        .is_none_or(|entry| entry["type"] != "message")
        || leaf_id.is_none()
    {
        return Err(not_persisted.to_owned());
    }
    let mut value = serde_json::json!({"outcome":"edited","entry":entry,"leafId":leaf_id});
    if let Some(summary) = result
        .summary_entry
        .and_then(|entry| entry["id"].as_str().map(str::to_owned))
    {
        value["summaryEntryId"] = summary.into();
    }
    Ok(Some(value))
}
/// senpi's `error()` default: `edit_user_message`/`navigate_tree` failures carry `errorData: { leafId }`.
fn default_error_data(session: &AgentSession, command: &str) -> Option<serde_json::Value> {
    matches!(command,"edit_user_message"|"navigate_tree").then(||serde_json::json!({"leafId":session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned))}))
}
/// The `{ cancelled, reason? }` wire shape of `reload` and `check_reload_veto` (rpc-types.ts).
fn reload_veto_json(decision: &maho_ext_api::ReloadVetoDecision) -> serde_json::Value {
    let mut value = serde_json::json!({"cancelled":decision.cancelled});
    if let Some(reason) = &decision.reason {
        value["reason"] = reason.clone().into();
    }
    value
}
/// The prompt command runs outside `handle_session_command_with_sink`: that function's poll frame is hundreds of KiB
/// in an unoptimized build, and keeping it on the stack beneath the whole agent run overflows a 2 MiB test thread.
async fn run_prompt_command(
    session: &AgentSession,
    command: &RpcCommand,
    sink: Option<RpcRecordSink>,
) -> Option<RpcResponse> {
    let RpcCommandBody::Prompt{message,images,streaming_behavior,thinking_level,session_title_prompt,expand_prompt_templates}=&command.body else{unreachable!("only prompt commands are routed here")};
    {
            let options=(||{
                if thinking_level.is_some()&&session.is_streaming()&&streaming_behavior.is_some(){return Err("Cannot set thinkingLevel on a queued prompt; set it after the current turn completes.".to_owned());}
                let images=images.as_ref().map(|images|images.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<maho_ai::types::ImageContent>,_>>()).transpose().map_err(|error|error.to_string())?;
                let thinking_level=thinking_level.as_ref().map(|level|serde_json::from_value(level.clone().into())).transpose().map_err(|error|error.to_string())?;
                let session_title_prompt=match session_title_prompt{Some(crate::rpc_types::SessionTitlePrompt::Text(text))=>Some(maho_core::agent_session::SessionTitlePrompt::Text(text.clone())),Some(crate::rpc_types::SessionTitlePrompt::Enabled(false))=>Some(maho_core::agent_session::SessionTitlePrompt::Disabled),None=>None,Some(crate::rpc_types::SessionTitlePrompt::Enabled(true))=>return Err("sessionTitlePrompt must be a string or false".into())};
                Ok(maho_core::agent_session::PromptOptions{images,thinking_level,session_title_prompt,expand_prompt_templates:*expand_prompt_templates,streaming_behavior:streaming_behavior.map(|behavior|match behavior{crate::rpc_types::StreamingBehavior::Steer=>maho_ext_api::StreamingBehavior::Steer,crate::rpc_types::StreamingBehavior::FollowUp=>maho_ext_api::StreamingBehavior::FollowUp}),source:Some(maho_ext_api::InputSource::Rpc),..Default::default()})
            })();
            let admitted=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let result=match options{Err(error)=>Err(error),Ok(mut options)=>{
                if let Some(sink)=sink{
                    let admitted=admitted.clone();let id=command.id.clone();let session_id=command.session_id.clone();
                    options.prompt_admitted=Some(std::sync::Arc::new(move|disposition|{
                        if admitted.swap(true,std::sync::atomic::Ordering::SeqCst){return;}
                        let disposition=match disposition{maho_core::agent_session::PromptDisposition::Started=>"started",maho_core::agent_session::PromptDisposition::Queued=>"queued",maho_core::agent_session::PromptDisposition::Handled=>"handled"};
                        let mut record=serde_json::json!({"type":"response","command":"prompt","success":true,"data":{"disposition":disposition}});
                        if let Some(id)=&id{record["id"]=id.clone().into();}
                        if let Some(session_id)=&session_id{record["sessionId"]=session_id.clone().into();}
                        sink(record);
                    }));
                }
                // Boxed: the prompt future is large and would otherwise be inlined into this command future's stack frame.
                Box::pin(session.prompt(message,options)).await.map(|disposition|Some(serde_json::json!({"disposition":match disposition{maho_core::agent_session::PromptDisposition::Started=>"started",maho_core::agent_session::PromptDisposition::Queued=>"queued",maho_core::agent_session::PromptDisposition::Handled=>"handled"}})))
            }};
            if admitted.load(std::sync::atomic::Ordering::SeqCst){return None;}
            Some(command_response(session,command,"prompt",result))
    }
}
fn command_response(session: &AgentSession, command: &RpcCommand, kind: &str, result: Result<Option<serde_json::Value>,String>) -> RpcResponse {
    RpcResponse {
        id: command.id.clone(),
        record_type: ResponseRecordType::Response,
        command: kind.into(),
        session_id: command.session_id.clone(),
        result: match result {
            Ok(data) => RpcResponseResult::Success { data },
            Err(error) => RpcResponseResult::Error {
                error,
                error_code: None,
                error_data: default_error_data(session, kind),
            },
        },
    }
}
async fn handle_session_command_with_sink(
    session: &AgentSession,
    command: &RpcCommand,
    sink: Option<RpcRecordSink>,
    active_logins: Option<ActiveLogins>,
    ui: Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>,
    last_abort_source: Option<&LastAbortSource>,
) -> Option<RpcResponse> {
    let (kind,result)=match &command.body{
        RpcCommandBody::HoldInput{..}|RpcCommandBody::WithdrawHeldInput{..}|RpcCommandBody::PauseForRun{..}|RpcCommandBody::ResumeForRun{..}=>{
            let value=serde_json::to_value(command).unwrap_or(serde_json::Value::Null);
            let kind=match &command.body{RpcCommandBody::HoldInput{..}=>"hold_input",RpcCommandBody::WithdrawHeldInput{..}=>"withdraw_held_input",RpcCommandBody::PauseForRun{..}=>"pause_for_run",_=>"resume_for_run"};
            let result=match kind{"hold_input"=>crate::held_input_commands::run_hold_input(session,&value,false,false),"pause_for_run"=>crate::run_hold_commands::run_pause_for_run(session,&value,false).await,"resume_for_run"=>crate::run_hold_commands::run_resume_for_run(session,&value,false).await,_=>Err("withdraw_held_input_requires_interactive_client".into())};
            (kind,result.map(Some))
        },
        RpcCommandBody::HostResponse{..}=>("host_response",Err("unknown_host_request".into())),
        RpcCommandBody::ArmToolBarrier { spec } => ("arm_tool_barrier",serde_json::from_value(spec.clone()).map_err(|e|e.to_string()).and_then(|spec|session.arm_tool_barrier(spec)).map(|()|None)),
        RpcCommandBody::ReleaseToolBarrier { barrier_id } => ("release_tool_barrier",Ok(Some(serde_json::json!({"released":session.release_tool_barrier(barrier_id)})))),
        RpcCommandBody::Prompt{..}=>unreachable!("prompt commands are routed to run_prompt_command"),
        RpcCommandBody::SwitchSession{session_path,cwd_override}=>match session.switch_session_with_cwd_coded(session_path,cwd_override.as_deref()).await{
            Ok(switched)=>("switch_session",Ok(Some(serde_json::json!({"cancelled":!switched})))),
            Err(error)=>return Some(session_switch_failure(command,"switch_session",error)),
        },
        RpcCommandBody::Fork{entry_id,position}=>{
            let include_entry=matches!(position,Some(crate::rpc_types::ForkPosition::At));
            ("fork",session.fork(entry_id,include_entry).await.map(|result|Some(serde_json::json!({"text":result.editor_text,"cancelled":result.cancelled}))))
        },
        RpcCommandBody::Clone=>{
            let leaf_id=session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned));
            let Some(leaf_id)=leaf_id else{return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"clone".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Cannot clone session: no current entry selected".into(),error_code:None,error_data:None}});};
            ("clone",session.fork(&leaf_id,true).await.map(|result|Some(serde_json::json!({"cancelled":result.cancelled}))))
        },
        RpcCommandBody::NewSession{parent_session}=>
            ("new_session",session.new_session(Some(maho_core::session_manager::NewSessionOptions{parent_session:parent_session.clone(),..Default::default()})).await.map(|changed|Some(serde_json::json!({"cancelled":!changed})))),
        RpcCommandBody::Abort=>{
            let mut abort=Box::pin(session.abort());
            let completed=std::future::poll_fn(|context|std::task::Poll::Ready(abort.as_mut().poll(context).is_ready())).await;
            if !completed&&let Some(sink)=sink{
                let mut record=serde_json::json!({"type":"response","command":"abort","success":true});
                if let Some(id)=&command.id{record["id"]=id.clone().into();}
                if let Some(id)=&command.session_id{record["sessionId"]=id.clone().into();}
                sink(record);abort.await;return None;
            }
            if !completed{abort.await;}
            ("abort",Ok(None))
        },
        RpcCommandBody::Reload=>("reload",session.reload().await.map(|decision|Some(reload_veto_json(&decision)))),
        RpcCommandBody::SetFavoriteModels{models}|RpcCommandBody::SetScopedModels{models}=>{
            #[derive(serde::Deserialize)]#[serde(rename_all="camelCase")]struct WireModel{model:maho_ai::types::Model,thinking_level:Option<maho_ai::types::ThinkingLevel>,thinking_selection:Option<maho_ai::types::ThinkingSelection>,service_tier:Option<String>}
            let parsed=models.iter().cloned().map(|value|{
                let entry:WireModel=serde_json::from_value(value).map_err(|error|error.to_string())?;
                let service_tier=entry.service_tier.map(|tier|match tier.as_str(){"auto"=>Ok(maho_ext_api::ServiceTier::Auto),"flex"=>Ok(maho_ext_api::ServiceTier::Flex),"priority"=>Ok(maho_ext_api::ServiceTier::Priority),"ultrafast"=>Ok(maho_ext_api::ServiceTier::Ultrafast),_=>Err(format!("Unknown service tier: {tier}"))}).transpose()?;
                Ok(maho_core::agent_session::SessionModelEntry{model:entry.model,thinking_level:entry.thinking_level,thinking_selection:entry.thinking_selection,service_tier})
            }).collect::<Result<Vec<_>,String>>();
            let favorite=matches!(&command.body,RpcCommandBody::SetFavoriteModels{..});
            (if favorite{"set_favorite_models"}else{"set_scoped_models"},parsed.map(|models|{if favorite{session.set_favorite_models(models);}else{session.set_scoped_models(models);}None}))
        },
        RpcCommandBody::AppendSessionEntry{entry}=>{session.append_session_entry(entry.clone());("append_session_entry",Ok(None))},
        RpcCommandBody::AppendUserMessage{content}=>{
            let message=serde_json::json!({"role":"user","content":content,"timestamp":maho_ai::utils::diagnostics::now_ms()});
            session.with_session_manager_mut(|manager|{manager.append_message(message);});
            let messages=session.with_session_manager(|manager|manager.current_context().messages);
            let agent_messages=messages.iter().filter_map(|message|serde_json::from_value(message.clone()).ok()).collect();
            session.agent().set_messages(agent_messages);
            ("append_user_message",Ok(None))
        },
        RpcCommandBody::SendCustomMessage{custom_type,content,display,details,trigger_turn,deliver_as}=>{
            let content=serde_json::from_value::<maho_agent::harness::messages::CustomMessageContent>(if content.is_null(){serde_json::json!([])}else{content.clone()}).map_err(|error:serde_json::Error|error.to_string());
            let result=match content{Err(error)=>Err(error),Ok(content)=>session.send_custom_message_content(custom_type.clone(),content,*display,details.clone(),maho_ext_api::SendMessageOptions{trigger_turn:trigger_turn.unwrap_or_else(||session.is_streaming()),deliver_as:deliver_as.map(|mode|match mode{crate::rpc_types::DeliveryMode::Steer=>maho_ext_api::DeliverAs::Steer,crate::rpc_types::DeliveryMode::FollowUp=>maho_ext_api::DeliverAs::FollowUp,crate::rpc_types::DeliveryMode::NextTurn=>maho_ext_api::DeliverAs::NextTurn})}).await};
            ("send_custom_message",result.map(|()|None))
        },
        RpcCommandBody::GetFastMode=>("get_fast_mode",Ok(Some(serde_json::json!({"enabled":session.is_fast_mode_active(),"serviceTier":session.effective_service_tier().map(|tier|match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority", maho_ext_api::ServiceTier::Ultrafast=>"ultrafast"})})))),
        RpcCommandBody::SetFastMode{enabled}=>{
            // Same entry point as /fast: persistence, `-fast` key normalization and pin precedence live in apply_fast_mode.
            let result=match session.apply_fast_mode(*enabled).await{
                Ok(result) if !result.applied=>Err(result.message),
                Ok(result)=>{
                    let memory=result.memory_model.unwrap_or_else(||session.model());
                    Ok(Some(serde_json::json!({"enabled":result.enabled,"serviceTier":match result.recorded_tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority", maho_ext_api::ServiceTier::Ultrafast=>"ultrafast"},"provider":memory.provider,"modelId":memory.id})))
                }
                Err(error)=>Err(error.message),
            };
            ("set_fast_mode",result)
        },
        RpcCommandBody::CleanupBashOutput{path}=>("cleanup_bash_output",session.cleanup_bash_output(std::path::Path::new(path)).await.map(|()|None)),
        RpcCommandBody::ExportJsonl{output_path}=>("export_jsonl",session.export_to_jsonl(output_path.as_deref()).map(|path|Some(serde_json::json!({"path":path}))).map_err(|error|error.to_string())),
        RpcCommandBody::ExportHtml{output_path,theme_name}=> {
            ("export_html", session.export_to_html(output_path.as_deref(), theme_name.as_deref()).map(|path|Some(serde_json::json!({"path":path}))))
        },
        RpcCommandBody::Steer{message,images,enqueue_order}|RpcCommandBody::FollowUp{message,images,enqueue_order}=>{
            let kind=if matches!(&command.body,RpcCommandBody::Steer{..}){"steer"}else{"follow_up"};
            let images=images.as_ref().map(|images|images.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<maho_ai::types::ImageContent>,_>>()).transpose();
            let result=match images{Err(error)=>Err(error.to_string()),Ok(images)=>{
                let options=QueuedInputOptions{enqueue_order:enqueue_order.map(|order|order as u64),source:Some(maho_ext_api::InputSource::Rpc),from_prompt:false};
                if kind=="steer"{session.steer(message,images,options).await}else{session.follow_up(message,images,options).await}
            }};
            (kind,result.map(|()|None))
        },
        RpcCommandBody::GetSteeringMessages=>("get_steering_messages",Ok(Some(serde_json::json!({"messages":session.get_steering_messages()})))),
        RpcCommandBody::GetFollowUpMessages=>("get_follow_up_messages",Ok(Some(serde_json::json!({"messages":session.get_follow_up_messages()})))),
        RpcCommandBody::ClearQueue{abort_will_follow}=>{let cleared=session.clear_queue(abort_will_follow.unwrap_or(false));let ordered=cleared.ordered.iter().map(|input|serde_json::json!({"text":input.text,"mode":match input.mode{maho_ext_api::StreamingBehavior::Steer=>"steer",maho_ext_api::StreamingBehavior::FollowUp=>"followUp"},"enqueueOrder":input.enqueue_order})).collect::<Vec<_>>();("clear_queue",Ok(Some(serde_json::json!({"steering":cleared.steering,"followUp":cleared.follow_up,"ordered":ordered}))))},
        RpcCommandBody::SetSessionName{name}=>{let name=name.trim();let result=if name.is_empty(){Err("Session name cannot be empty".into())}else{session.set_session_name(name);Ok(None)};("set_session_name",result)},
        RpcCommandBody::GetLastAssistantText=>("get_last_assistant_text",Ok(Some(session.get_last_assistant_text().map_or_else(||serde_json::json!({}),|text|serde_json::json!({"text":text}))))),
        RpcCommandBody::GetMessages=>{let messages=session.messages().into_iter().map(|message|match message{maho_agent::types::AgentMessage::Llm(message)=>serde_json::to_value(message),maho_agent::types::AgentMessage::Custom(message)=>match message{maho_agent::types::CustomAgentMessage::BashExecution(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::Custom(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::BranchSummary(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::CompactionSummary(message)=>serde_json::to_value(message)}}).collect::<Result<Vec<_>,_>>();("get_messages",messages.map(|messages|Some(serde_json::json!({"messages":messages}))).map_err(|error|error.to_string()))},
        RpcCommandBody::SetAutoCompaction{enabled}=>{session.set_auto_compaction_enabled(*enabled);("set_auto_compaction",Ok(None))},
        RpcCommandBody::SetAutoRetry{enabled}=>("set_auto_retry",session.set_auto_retry_enabled(*enabled).map(|()|None)),
        RpcCommandBody::AbortRetry=>{session.abort_retry();("abort_retry",Ok(None))},
        RpcCommandBody::AbortCompaction=>{session.abort_compaction();("abort_compaction",Ok(None))},
        RpcCommandBody::CheckReloadVeto=>("check_reload_veto",session.check_reload_veto().await.map(|decision|Some(reload_veto_json(&decision)))),
        RpcCommandBody::AbortBranchSummary=>{session.abort_branch_summary();("abort_branch_summary",Ok(None))},
        RpcCommandBody::AbortBash=>{session.abort_bash();("abort_bash",Ok(None))},
        RpcCommandBody::SetSteeringMode{mode}|RpcCommandBody::SetFollowUpMode{mode}=>{let mode=match mode{crate::rpc_types::QueueMode::All=>maho_agent::types::QueueMode::All,crate::rpc_types::QueueMode::OneAtATime=>maho_agent::types::QueueMode::OneAtATime};let kind=if matches!(&command.body,RpcCommandBody::SetSteeringMode{..}){session.set_steering_mode(mode);"set_steering_mode"}else{session.set_follow_up_mode(mode);"set_follow_up_mode"};(kind,Ok(None))},
        RpcCommandBody::GetAvailableThinkingLevels=>("get_available_thinking_levels",Ok(Some(serde_json::json!({"levels":session.get_available_thinking_levels()})))),
        RpcCommandBody::CycleThinkingLevel=>("cycle_thinking_level",Ok(Some(session.cycle_thinking_level().map_or(serde_json::Value::Null,|level|serde_json::json!({"level":level}))))),
        RpcCommandBody::SetThinkingLevel{level,scope}=>{
            let result=serde_json::from_value::<maho_ai::types::ModelThinkingLevel>(level.clone().into()).map_err(|error|error.to_string()).and_then(|parsed|{
                if scope.is_some(){if !session.get_available_thinking_levels().contains(&parsed){return Err(format!("Thinking level {level} is not supported by the active model."));}session.set_session_thinking_level(parsed);}else{session.set_thinking_level(parsed);}
                Ok(None)
            });("set_thinking_level",result)
        },
        RpcCommandBody::Compact{custom_instructions}=>("compact",session.compact(custom_instructions.as_deref()).await.map(|result|{
            let mut value=serde_json::json!({"summary":result.summary,"firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":result.tokens_before});
            if let Some(tokens)=result.estimated_tokens_after{value["estimatedTokensAfter"]=tokens.into();}
            if let Some(usage)=result.usage{value["usage"]=serde_json::json!(usage);}
            if let Some(details)=result.details{value["details"]=details;}
            Some(value)
        })),
        RpcCommandBody::GetAvailableModels=>{let models=session.model_registry().get_available().into_iter().map(|model|{let mut value=serde_json::json!(model);value["supportedThinkingLevels"]=serde_json::json!(maho_core::thinking_levels::get_supported_thinking_levels(&model));value}).collect::<Vec<_>>();("get_available_models",Ok(Some(serde_json::json!({"models":models}))))},
        RpcCommandBody::SetModel{provider,model_id}=>{
            let model=session.model_registry().get_available().into_iter().find(|model|&model.provider==provider&&&model.id==model_id);
            let result=if let Some(model)=model{session.set_model(model.clone()).await.map(|change|{let mut value=serde_json::json!(model);if let Some(name)=change.and_then(|change|change.system_prompt_name){value["systemPromptName"]=name.into();}Some(value)})}else{Err(format!("Model not found: {provider}/{model_id}"))};("set_model",result)
        },
        RpcCommandBody::CycleModel{direction}=>("cycle_model",session.cycle_model(!matches!(direction,Some(crate::rpc_types::ModelDirection::Backward))).await.map(|result|Some(result.map_or(serde_json::Value::Null,|result|{
            let mut value=serde_json::json!({"model":result.model,"thinkingLevel":result.thinking_level,"isScoped":result.is_scoped,"skippedModels":result.skipped_models});
            if let Some(change)=result.system_prompt_change{
                let mut event=serde_json::json!({"type":"system_prompt_change","systemPrompt":change.system_prompt,"previousSystemPrompt":change.previous_system_prompt,"model":change.model,"source":"model_select"});
                if let Some(model)=change.previous_model{event["previousModel"]=serde_json::json!(model);}
                if let Some(name)=change.system_prompt_name{event["systemPromptName"]=name.into();}
                value["systemPromptChange"]=event;
            }
            value
        })))),
        RpcCommandBody::GetForkMessages=>{let messages=session.get_user_messages_for_forking().into_iter().map(|(entry_id,text)|serde_json::json!({"entryId":entry_id,"text":text})).collect::<Vec<_>>();("get_fork_messages",Ok(Some(serde_json::json!({"messages":messages}))))},
        RpcCommandBody::GetSessionStats=>{let stats=session.get_session_stats();let mut value=serde_json::json!({"sessionId":stats.session_id,"userMessages":stats.user_messages,"assistantMessages":stats.assistant_messages,"toolCalls":stats.tool_calls,"toolResults":stats.tool_results,"totalMessages":stats.total_messages,"tokens":{"input":stats.tokens.input,"output":stats.tokens.output,"cacheRead":stats.tokens.cache_read,"cacheWrite":stats.tokens.cache_write,"total":stats.tokens.total},"cost":stats.cost});
            if let Some(file)=stats.session_file{value["sessionFile"]=file.into();}
            if let Some(usage)=stats.context_usage{value["contextUsage"]=serde_json::json!({"tokens":usage.tokens,"contextWindow":usage.context_window,"percent":usage.percent});}
            ("get_session_stats",Ok(Some(value)))
        },
        RpcCommandBody::GetEntries{since}=>{let since=since.clone();("get_entries",session.with_session_manager(|manager|{let mut entries=manager.entries();if let Some(since)=since.as_deref(){let index=entries.iter().position(|entry|entry["id"].as_str()==Some(since)).ok_or_else(||format!("Entry not found: {since}"))?;entries.drain(..=index);}Ok(Some(serde_json::json!({"entries":entries,"leafId":manager.leaf_id()})))}))},
        RpcCommandBody::NavigateTree{entry_id,target_id,intent,expected_leaf_id,summarize,custom_instructions,replace_instructions,label}=>{
            if entry_id.is_some()&&target_id.is_some(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error_data:default_error_data(session,"navigate_tree"),error:"navigate_tree takes either entryId or targetId, not both".into(),error_code:None}});}
            if entry_id.is_none()&&target_id.is_none(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error_data:default_error_data(session,"navigate_tree"),error:"navigate_tree requires entryId or targetId".into(),error_code:None}});}
            let selected=entry_id.as_deref().or(target_id.as_deref()).unwrap_or_default();
            if selected.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error_data:default_error_data(session,"navigate_tree"),error:"navigate_tree requires a non-empty entryId or targetId".into(),error_code:None}});}
            let result=session.navigate_tree_coded(selected,maho_core::agent_session::TreeNavigationOptions{intent:intent.map(|intent|match intent{crate::rpc_types::NavigationIntent::Select=>maho_core::agent_session::TreeNavigationIntent::Select,crate::rpc_types::NavigationIntent::Resume=>maho_core::agent_session::TreeNavigationIntent::Resume}),summarize:*summarize,custom_instructions:custom_instructions.clone(),replace_instructions:*replace_instructions,label:label.clone(),expected_leaf_id:expected_leaf_id.clone()}).await;
            let result=match result{Ok(result)=>result,Err(error)=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:error.message,error_code:error.code.map(str::to_owned),error_data:default_error_data(session,"navigate_tree")}})};
            let result=Ok(result).map(|result:maho_core::agent_session::AssistantEditResult|{let leaf_id=session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned));if target_id.is_some(){let mut value=serde_json::json!({"leafId":leaf_id,"cancelled":result.cancelled});
                if let Some(aborted)=result.aborted{value["aborted"]=aborted.into();}
                if let Some(editor_text)=result.editor_text{value["editorText"]=editor_text.into();}
                if let Some(summary_entry)=result.summary_entry{value["summaryEntry"]=summary_entry;}value}else if result.cancelled{let mut value=serde_json::json!({"outcome":"cancelled","leafId":leaf_id});if let Some(aborted)=result.aborted{value["aborted"]=aborted.into();}value}else{let mut value=serde_json::json!({"outcome":"navigated","leafId":leaf_id});if let Some(editor_text)=result.editor_text{value["editorText"]=editor_text.into();}
                if let Some(summary_entry)=result.summary_entry&&let Some(id)=summary_entry["id"].as_str(){value["summaryEntryId"]=id.into();}value}}).map(Some);("navigate_tree",result)
        },
        RpcCommandBody::SetLabel{entry_id,label}=>{let result=session.with_session_manager_mut(|manager|manager.append_label(entry_id,label.as_deref())).map(|_|None);("set_label",result)},
        RpcCommandBody::Bash{command:bash_command,exclude_from_context,execution_id,..}=>{
            // senpi brackets a routed (multi-session) bash with `bash_start`/`bash_end`; the routing
            // id reaches this handler as the command's `sessionId`.
            let routed=sink.as_ref().filter(|_|command.session_id.is_some());
            if let Some(sink)=routed{sink(serde_json::json!({"type":"bash_start"}));}
            let result=session.execute_user_bash(bash_command,None,exclude_from_context.unwrap_or(false),execution_id.clone()).await;
            if let Some(sink)=routed{sink(serde_json::json!({"type":"bash_end"}));}
            ("bash",result.map(|result|Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null))))
        },
        RpcCommandBody::RecordBashResult{command,result,exclude_from_context}=>{
            #[derive(serde::Deserialize)]#[serde(rename_all="camelCase")]struct WireResult{output:String,exit_code:Option<i32>,cancelled:bool,truncated:bool,full_output_path:Option<std::path::PathBuf>}
            let response=serde_json::from_value::<WireResult>(result.clone()).map_err(|error|error.to_string()).map(|result|{let parsed=maho_tools::bash_executor::BashResult{output:result.output,exit_code:result.exit_code,cancelled:result.cancelled,truncated:result.truncated,full_output_path:result.full_output_path};session.record_bash_result(command,&parsed,exclude_from_context.unwrap_or(false));None});("record_bash_result",response)
        },
        RpcCommandBody::GetCommands=>{
            // Upstream `case "get_commands"`: `buildRpcCommandsForSession(session)` classifies each of the
            // three groups by its real source; the session's `command_groups` reads the same three.
            let commands=build_rpc_commands_for_session(session);
            ("get_commands",Ok(Some(serde_json::json!({"commands":commands}))))
        },
        RpcCommandBody::GetAuthProviders=>{
            let registry=session.model_registry();
            let providers=maho_core::auth_providers::build_login_provider_infos(registry,Some(maho_core::auth_storage::CredentialKind::Oauth)).and_then(|oauth| {
                let api_keys=maho_core::auth_providers::build_login_provider_infos(registry,Some(maho_core::auth_storage::CredentialKind::ApiKey))?;
                Ok(oauth.into_iter().chain(api_keys).map(|info|{
                let status=registry.get_provider_auth_status(&info.id);
                serde_json::json!({"id":info.id,"name":info.name,"authType":match info.auth_type{maho_core::auth_storage::CredentialKind::Oauth=>"oauth",maho_core::auth_storage::CredentialKind::ApiKey=>"api_key"},"status":{"configured":status.configured,"source":status.source,"label":status.label}})
                }).collect::<Vec<_>>())
            }).map_err(|error|error.to_string());
            ("get_auth_providers",providers.map(|providers|Some(serde_json::json!({"providers":providers}))))
        },
        RpcCommandBody::LoginStart{provider}=>{
            let sink=sink.unwrap_or_else(||std::sync::Arc::new(|_|{}));
            let active_logins=active_logins.unwrap_or_else(||std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())));
            let controller=maho_ai::utils::abort::AbortController::new();
            if let Some(previous)=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(provider.clone(),controller.clone()){previous.abort(None);}
            let provider=provider.clone();let storage=session.model_registry().auth_storage.clone();let mut runtime=session.model_registry().model_runtime.clone();let active_logins=active_logins.clone();let event_sink=sink.clone();let ui=ui.unwrap_or_else(||std::sync::Arc::new(crate::connection_handler::RpcExtensionUi::new(event_sink.clone(),Default::default(),Default::default(),Vec::new())));
            tokio::spawn(async move{
                let settled=maho_ai::utils::abort::AbortController::new();
                let combined=maho_ai::utils::abort::AbortController::new();
                let combined_signal=combined.signal();
                let login_signal=controller.signal();
                let settled_signal=settled.signal();
                let combined_for_login=combined.clone();
                let _login_listener=login_signal.add_abort_listener(move |_|combined_for_login.abort(None));
                let combined_for_settled=combined.clone();
                let _settled_listener=settled_signal.add_abort_listener(move |_|combined_for_settled.abort(None));
                let callbacks=std::sync::Arc::new(crate::login_prompts::RpcLoginPromptCallbacks::with_sink(combined_signal,provider.clone(),event_sink.clone(),ui));
                let result=storage.login(&provider,callbacks).await;
                if result.is_ok(){let _=runtime.refresh(Default::default()).await;}
                if provider=="anthropic-subscription"&&result.is_ok(){maho_core::emit_provider_accounts_changed(&provider);}
                let mut end=serde_json::json!({"type":"auth_login_end","provider":provider,"success":result.is_ok()});
                if let Err(error)=result{end["error"]=error.into();}
                event_sink(end);
                settled.abort(None);
                let mut logins=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if logins.get(&provider).is_some_and(|current|current.signal().same_signal(&controller.signal())){logins.remove(&provider);}
            });
            return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"login_start".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Success{data:None}});
        },
        RpcCommandBody::LoginCancel{provider}=>{
            if let Some(active_logins)=active_logins && let Some(controller)=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider){controller.abort(None);}
            ("login_cancel",Ok(None))
        },
        RpcCommandBody::LoginApiKey{provider,key}=>{
            let result=session.model_registry().auth_storage.set(provider,Some(serde_json::json!({"type":"api_key","key":key}))).map(|()|{let registry=session.model_registry();let mut runtime=registry.model_runtime.clone();tokio::spawn(async move{let _=runtime.refresh(Default::default()).await;});None});
            ("login_api_key",result)
        },
        RpcCommandBody::Logout{provider}=>{
            let result=session.model_registry().auth_storage.logout(provider).map(|()|{let registry=session.model_registry();let mut runtime=registry.model_runtime.clone();tokio::spawn(async move{let _=runtime.refresh(Default::default()).await;});None});
            ("logout",result)
        },
        RpcCommandBody::GetProviderAccounts{provider}=>{
            let result=session.model_registry().get_credential_accounts(provider,&session.agent_dir()).await
                .map(|accounts|Some(serde_json::json!({"accounts":accounts.into_iter().map(|account|serde_json::json!({
                    "name":account.name,"displayName":account.display_name,"source":account.source.as_str(),"blocked":account.blocked,"pinned":account.pinned,
                })).collect::<Vec<_>>() })));
            ("get_provider_accounts",result)
        },
        RpcCommandBody::AccountPin{provider,name}=>{
            let result=session.model_registry().pin_credential_account(provider,name.as_deref(),&session.agent_dir()).await.map(|()|None);
            ("account_pin",result)
        },
        RpcCommandBody::AccountRemove{provider,name}=>{
            let result=session.model_registry().remove_credential_account(provider,name,&session.agent_dir()).await.map(|()|None);
            ("account_remove",result)
        },
        RpcCommandBody::GetState=>("get_state",Ok(Some(build_rpc_session_state_with_dialogs(session,current_abort_source(session,last_abort_source),Vec::new())))),
        RpcCommandBody::ResumeSession=>("resume_session",session.resume_session().await.map(|()|None)),
        // The session keeps no draft-activity state in this port; the validated command is acknowledged (kind is a closed enum).
        RpcCommandBody::RecordUserActivity{..}=>("record_user_activity",Ok(None)),
        RpcCommandBody::GetTree => {
            fn tree_node(node: maho_core::session_manager::SessionTreeNode) -> serde_json::Value {
                let mut value = serde_json::json!({"entry":node.entry,"children":node.children.into_iter().map(tree_node).collect::<Vec<_>>()});
                if let Some(label) = node.label { value["label"] = label.into(); }
                if let Some(timestamp) = node.label_timestamp { value["labelTimestamp"] = timestamp.into(); }
                value
            }
            let (tree, leaf_id) = session.with_session_manager(|manager| {
                let tree = manager.get_tree().into_iter().map(tree_node).collect::<Vec<_>>();
                (tree, manager.leaf_id().map(str::to_owned))
            });
            ("get_tree", Ok(Some(serde_json::json!({"tree":tree,"leafId":leaf_id}))))
        },
        RpcCommandBody::ImportJsonl{input_path,cwd_override}=>match session.import_from_jsonl_coded(input_path,cwd_override.as_deref()).await{
            Ok(imported)=>("import_jsonl",Ok(Some(serde_json::json!({"cancelled":!imported})))),
            Err(error)=>return Some(session_switch_failure(command,"import_jsonl",error)),
        },
        RpcCommandBody::EditAssistantMessage{entry_id,text,expected_leaf_id,summarize,custom_instructions}=>{
            if entry_id.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_assistant_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"edit_assistant_message requires a non-empty entryId and a text string".into(),error_code:None,error_data:None}});}
            let result=session.edit_assistant_message_coded(entry_id,text,maho_core::agent_session::TreeNavigationOptions{expected_leaf_id:expected_leaf_id.clone(),summarize:*summarize,custom_instructions:custom_instructions.clone(),..Default::default()}).await;
            let result=match result{Ok(result)=>Ok(result),Err(error)=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_assistant_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:error.message,error_code:error.code.map(str::to_owned),error_data:None}})};
            ("edit_assistant_message",edit_response(session,result,"Edited assistant entry was not persisted"))
        },
        RpcCommandBody::EditUserMessage{entry_id,text,expected_leaf_id,summarize,custom_instructions}=>{
            if entry_id.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_user_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"edit_user_message requires a non-empty entryId and a text string".into(),error_code:None,error_data:default_error_data(session,"edit_user_message")}});}
            let result=session.edit_user_message_coded(entry_id,text,maho_core::agent_session::TreeNavigationOptions{expected_leaf_id:expected_leaf_id.clone(),summarize:*summarize,custom_instructions:custom_instructions.clone(),..Default::default()}).await;
            let result=match result{Ok(result)=>Ok(result),Err(error)=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_user_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:error.message,error_code:error.code.map(str::to_owned),error_data:default_error_data(session,"edit_user_message")}})};
            ("edit_user_message",edit_response(session,result,"Edited user entry was not persisted"))
        },
        RpcCommandBody::GetMedia{tool_call_id,content_index}=>{
            let index=if content_index.is_finite()&&content_index.fract()==0.0&&*content_index>=0.0{*content_index as usize}else{usize::MAX};
            // Durable session entries first (they survive compaction of the live window), then live messages.
            let durable=session.with_session_manager(|manager|manager.entries()).into_iter().find_map(|entry|{
                let message=(entry["type"]=="message").then_some(&entry["message"])?;
                (message["role"]=="toolResult"&&message["toolCallId"].as_str()==Some(tool_call_id.as_str())).then_some(())?;
                message["content"].get(index).filter(|block|block["type"]=="image").cloned()
            });
            let content=durable.or_else(||session.messages().into_iter().find_map(|message|{
                let result=message.as_tool_result()?;
                (result.tool_call_id==*tool_call_id).then_some(result.content.get(index)).flatten().and_then(|block|{
                    matches!(block,maho_ai::types::ContentBlock::Image(_)).then_some(serde_json::to_value(block).ok()).flatten()
                })
            }));
            let result=content.map(|content|Some(serde_json::json!({"toolCallId":tool_call_id,"contentIndex":content_index,"content":content}))).ok_or_else(||crate::rpc_types::RPC_ERROR_MEDIA_NOT_FOUND.to_owned());
            ("get_media",result)
        },
        RpcCommandBody::GetLoadedSurfaces=>{
            let result = async {
            let (respond, receive) = tokio::sync::oneshot::channel();
            let request = maho_ext_api::mcp_wire::McpControlInventoryRequest { session_id: session.session_id(), respond: std::sync::Mutex::new(Some(respond)) };
            session.emit_native_extension_event(maho_ext_api::mcp_wire::MCP_CONTROL_INVENTORY_REQUEST_EVENT, &request).await;
            let claimed = request.respond.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().is_none();
            let snapshot = if claimed { serde_json::from_value::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>(receive.await.map_err(|error|format!("MCP inventory request failed: {error}"))?).map_err(|error|format!("MCP inventory response invalid: {error}"))? } else { session.mcp_wire_status_snapshot::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>().unwrap_or_default() };
            let mcp_servers = snapshot.servers.iter().map(|server| {
                let status = server.status.as_ref().map(|status|serde_json::json!(status)).unwrap_or_else(||serde_json::json!(if server.server_info.is_some() { "connected" } else if server.auth_status == maho_ext_api::mcp_wire::McpWireAuthStatus::NotLoggedIn { "needs_auth" } else { "enabled" }));
                serde_json::json!({"name":server.name,"toolCount":server.tools.len(),"status":status,"authStatus":server.auth_status})
            }).collect::<Vec<_>>();
            let extensions=session.loaded_extension_surfaces().await;
            Ok(Some(serde_json::json!({"extensions":extensions,"mcpServers":mcp_servers})))
            }.await;
            ("get_loaded_surfaces",result)
        },
        RpcCommandBody::ExtensionRequest{name,data}=>{
            let name=name.trim();
            if name.is_empty(){("extension_request",Err("Extension RPC request name cannot be empty".into()))}
            else {("extension_request",session.extension_runner_request(name,data.clone().unwrap_or(serde_json::Value::Null)).await.map(Some))}
        },
        RpcCommandBody::SetClientInfo{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"set_client_info".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Success{data:None}}),
        RpcCommandBody::GetProtocolInfo=>{
            // Classic connection-handler `get_protocol_info`: the same identity as the multi host, `mode:"classic"`.
            let env=maho_core::config::current_env();
            let argv=std::env::args().skip(1).collect::<Vec<_>>();
            let cwd=std::env::current_dir().map(|cwd|cwd.to_string_lossy().into_owned()).unwrap_or_default();
            let mut data=crate::protocol_identity::host_launch_profile(&argv,&cwd).ok().map_or_else(||serde_json::json!({}),|profile|crate::protocol_identity::protocol_identity(profile,&env));
            let mut capabilities=vec!["multi_session".to_owned(),crate::custom_capability::AUTO_TITLE_SESSIONS_CAPABILITY.to_owned(),crate::custom_capability::MEDIA_PLACEHOLDERS_CAPABILITY.to_owned()];
            for capability in crate::custom_capability::parse_client_capabilities(maho_core::brand::env_value("RPC_CLIENT_CAPABILITIES",&env).as_deref()){if !capabilities.contains(&capability){capabilities.push(capability);}}
            data["protocolVersion"]=crate::host_decision::HOST_PROTOCOL_VERSION.into();
            data["serverVersion"]=maho_core::engine_build_identity::engine_build_identity().text.clone().into();
            data["capabilities"]=serde_json::json!(capabilities);
            data["mode"]="classic".into();
            return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"get_protocol_info".into(),session_id:None,result:RpcResponseResult::Success{data:Some(data)}});
        },
        RpcCommandBody::OpenSession{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"open_session".into(),session_id:None,result:RpcResponseResult::Error{error:crate::rpc_types::RPC_ERROR_MULTI_SESSION_DISABLED.into(),error_code:None,error_data:None}}),
        RpcCommandBody::CloseSession=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"close_session".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Unknown command: close_session".into(),error_code:None,error_data:None}}),
        RpcCommandBody::ReleaseSession{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"release_session".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Unknown command: release_session".into(),error_code:None,error_data:None}}),
        RpcCommandBody::SetSessionContext{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"set_session_context".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Unknown command: set_session_context".into(),error_code:None,error_data:None}}),
        RpcCommandBody::ListSessions{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"list_sessions".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Unknown command: list_sessions".into(),error_code:None,error_data:None}}),
    };
    Some(RpcResponse {
        id: command.id.clone(),
        record_type: ResponseRecordType::Response,
        command: kind.into(),
        session_id: command.session_id.clone(),
        result: match result {
            Ok(data) => RpcResponseResult::Success { data },
            Err(error) => RpcResponseResult::Error {
                error,
                error_code: None,
                error_data: default_error_data(session, kind),
            },
        },
    })
}
/// The RPC-side extension UI: dialogs become `extension_ui_request` frames answered by a
/// client's `extension_ui_response`; fire-and-forget methods only emit (senpi's
/// `createExtensionUIContext` inside `createRpcConnectionHandler`).
pub struct RpcExtensionUi {
    out: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    pending: std::sync::Arc<
        std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>,
    >,
    questions: std::sync::Arc<
        std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>,
    >,
    capabilities: Vec<String>,
    client_width: std::sync::atomic::AtomicUsize,
    live_renderers:
        std::sync::Mutex<std::collections::BTreeMap<String, (LiveRenderer, RenderedTarget)>>,
    retained_renderer_factories: std::sync::Mutex<
        std::collections::BTreeMap<String, (maho_ext_api::ComponentFactory, RenderedTarget)>,
    >,
}
/// Where a rendered component's lines go on the wire (senpi's three `createLiveComponentRenderer` emits).
#[derive(Clone, Copy)]
enum RenderedTarget {
    Widget(&'static str),
    Header,
    Footer,
}
impl RenderedTarget {
    fn record(self, key: &str, lines: Option<Vec<String>>) -> serde_json::Value {
        let id = uuid::Uuid::new_v4().to_string();
        match self {
            Self::Widget(placement) => {
                let mut record = serde_json::json!({"type":"extension_ui_request","id":id,"method":"setWidget","widgetKey":key,"widgetPlacement":placement});
                if let Some(lines) = lines {
                    record["widgetLines"] = lines.into();
                }
                record
            }
            Self::Header | Self::Footer => {
                let method = if matches!(self, Self::Header) {
                    "setHeader"
                } else {
                    "setFooter"
                };
                let mut record =
                    serde_json::json!({"type":"extension_ui_request","id":id,"method":method});
                if let Some(lines) = lines {
                    record["widgetLines"] = lines.into();
                }
                record
            }
        }
    }
    fn rendered(self, key: &str, lines: Vec<String>) -> serde_json::Value {
        let mut record = self.record(key, Some(lines));
        record[crate::session_event_fanout::RENDERED_COMPONENT_RECORD] = true.into();
        record
    }
}
const HEADER_RENDERER_KEY: &str = "__header__";
const FOOTER_RENDERER_KEY: &str = "__footer__";
/// Adaptation: native component factories return non-`Send` components, so the RPC renderer keeps
/// the (thread-safe) factory and instantiates a component per render, disposing it right after.
struct FactoryComponent {
    factory: maho_ext_api::ComponentFactory,
    theme: maho_ext_api::Theme,
}
impl maho_tui::tui::Component for FactoryComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut component = (self.factory)(&self.theme);
        let lines = component.render(width);
        component.dispose();
        lines
    }
}
type LiveRenderer = crate::widget_line_renderer::LiveComponentRenderer<FactoryComponent>;
impl RpcExtensionUi {
    pub fn new(
        out: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
        pending: std::sync::Arc<
            std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>,
        >,
        questions: std::sync::Arc<
            std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>,
        >,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            out,
            pending,
            questions,
            capabilities,
            client_width: std::sync::atomic::AtomicUsize::new(80),
            live_renderers: Default::default(),
            retained_renderer_factories: Default::default(),
        }
    }
    /// Live select/confirm/input/editor request frames in insertion order.
    pub fn pending_dialogs(&self) -> Vec<serde_json::Value> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot()
    }
    fn has_rendered_components(&self) -> bool {
        self.capabilities.iter().any(|capability| {
            capability == crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY
        })
    }
    fn dispose_renderer(&self, key: &str) {
        let removed = self
            .live_renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
        if let Some((mut renderer, _)) = removed {
            renderer.dispose();
        }
        self.retained_renderer_factories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }
    fn create_renderer(&self, key: &str) {
        let Some((factory, target)) = self
            .retained_renderer_factories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
        else {
            return;
        };
        let mut renderer = LiveRenderer::new(FactoryComponent {
            factory,
            theme: maho_ext_api::Theme::default(),
        });
        let lines = renderer.rerender(self.client_width.load(std::sync::atomic::Ordering::SeqCst));
        self.live_renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_owned(), (renderer, target));
        if let Some(lines) = lines {
            (self.out)(target.rendered(key, lines));
        }
    }
    /// Retain a component factory; render it now only for a `rendered_components` client.
    fn retain_renderer(
        &self,
        key: &str,
        factory: maho_ext_api::ComponentFactory,
        target: RenderedTarget,
    ) {
        self.dispose_renderer(key);
        self.retained_renderer_factories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_owned(), (factory, target));
        if self.has_rendered_components() {
            self.create_renderer(key);
        }
    }
    /// The client width live components render at (senpi `clientWidth`).
    pub fn set_client_width(&self, width: usize) {
        self.client_width
            .store(width, std::sync::atomic::Ordering::SeqCst);
    }
    /// Re-render every live component at the current width; unchanged frames are not re-emitted.
    pub fn rerender_live_components(&self) {
        let width = self.client_width.load(std::sync::atomic::Ordering::SeqCst);
        let records = self
            .live_renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
            .filter_map(|(key, (renderer, target))| {
                renderer
                    .rerender(width)
                    .map(|lines| target.rendered(key, lines))
            })
            .collect::<Vec<_>>();
        for record in records {
            (self.out)(record);
        }
    }
    /// senpi `rerenderComponents`: drop renderers for a client without `rendered_components`,
    /// otherwise materialize retained factories and re-render.
    pub fn rerender_components(&self) {
        if !self.has_rendered_components() {
            self.dispose_all_renderers();
            return;
        }
        let missing = self
            .retained_renderer_factories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in missing {
            let live = self
                .live_renderers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&key);
            if !live {
                self.create_renderer(&key);
            }
        }
        self.rerender_live_components();
    }
    pub fn dispose_all_renderers(&self) {
        let renderers = std::mem::take(
            &mut *self
                .live_renderers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (_, (mut renderer, _)) in renderers {
            renderer.dispose();
        }
    }
    pub fn select_grant(
        &self,
        title: &str,
        choices: &[String],
        opts: &maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'static, Option<String>> {
        self.ask(opts,None,serde_json::json!({"method":"select","title":title,"options":choices,"grantSelect":true}),|response|if response["cancelled"]==true{None}else{response["value"].as_str().map(str::to_owned)})
    }
    fn ask<T: 'static + Send>(
        &self,
        opts: &maho_ext_api::ExtensionUiDialogOptions,
        default: T,
        mut request: serde_json::Value,
        parse: impl Fn(serde_json::Value) -> T + Send + 'static,
    ) -> maho_ext_api::UiFuture<'static, T> {
        if opts
            .signal
            .as_ref()
            .is_some_and(|signal| signal.is_aborted())
        {
            return Box::pin(async move { default });
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        request["type"] = "extension_ui_request".into();
        request["id"] = id.clone().into();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_with_frame(id.clone(), sender, request.clone());
        if request["grantSelect"] == true {
            let connection = crate::multi_session_host::CURRENT_CONNECTION
                .try_with(Clone::clone)
                .ok()
                .flatten();
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .bind_grant(&id, connection);
        }
        (self.out)(request);
        let timeout = opts.timeout_ms;
        let signal = opts.signal.clone();
        let pending = self.pending.clone();
        Box::pin(async move {
            let mut abort = signal.as_ref().map(|signal| Box::pin(signal.cancelled()));
            let response = match (abort.as_mut(), timeout) {
                (Some(abort), Some(ms)) => {
                    tokio::select! {received=receiver=>received.ok(),_=abort=>None,()=tokio::time::sleep(std::time::Duration::from_millis(ms))=>None}
                }
                (Some(abort), None) => {
                    tokio::select! {received=receiver=>received.ok(),_=abort=>None}
                }
                (None, Some(ms)) => {
                    tokio::select! {received=receiver=>received.ok(),()=tokio::time::sleep(std::time::Duration::from_millis(ms))=>None}
                }
                (None, None) => receiver.await.ok(),
            };
            pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .delete(&id);
            match response {
                Some(Ok(response)) => parse(response),
                Some(Err(_)) | None => default,
            }
        })
    }
}
impl maho_ext_api::ExtensionUi for RpcExtensionUi {
    fn question<'a>(
        &'a self,
        request: maho_ext_api::QuestionRequest,
        options: maho_ext_api::QuestionOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::QuestionResponse> {
        if !self
            .capabilities
            .iter()
            .any(|capability| capability == crate::custom_capability::QUESTION_CAPABILITY)
        {
            let ui = self as &dyn maho_ext_api::ExtensionUi;
            return Box::pin(crate::connection_question_bridge::degrade_question(
                ui,
                request,
                options.dialog,
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let (frame, receiver) = self
            .questions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ask(request.clone(), None, now);
        (self.out)(frame);
        let questions = self.questions.clone();
        Box::pin(async move {
            match tokio::time::timeout(
                std::time::Duration::from_millis(request.timeout_ms),
                receiver,
            )
            .await
            {
                Ok(Ok(response)) => Ok(response),
                _ => {
                    let deadline = now.saturating_add(request.timeout_ms);
                    questions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .expire(deadline);
                    Ok(maho_ext_api::QuestionResponse {
                        status: maho_ext_api::QuestionStatus::TimedOut,
                        answers: Default::default(),
                        comment: None,
                        unanswered: request
                            .questions
                            .iter()
                            .map(|question| question.id.clone())
                            .collect(),
                        auto_resolved_after_ms: Some(request.timeout_ms),
                        cancel_origin: None,
                        resolved_by: None,
                    })
                }
            }
        })
    }
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        self.ask(&opts,None,serde_json::json!({"method":"select","title":title,"options":options,"timeout":opts.timeout_ms}),|response|{
            if response["cancelled"].as_bool()==Some(true){None}else{response["value"].as_str().map(str::to_owned)}
        })
    }
    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        self.ask(&opts,false,serde_json::json!({"method":"confirm","title":title,"message":message,"timeout":opts.timeout_ms}),|response|response["cancelled"].as_bool()!=Some(true)&&response["confirmed"].as_bool().unwrap_or(false))
    }
    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        self.ask(&opts,None,serde_json::json!({"method":"input","title":title,"placeholder":placeholder,"timeout":opts.timeout_ms}),|response|{
            if response["cancelled"].as_bool()==Some(true){None}else{response["value"].as_str().map(str::to_owned)}
        })
    }
    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async move {
            Ok(self
                .ask(
                    &maho_ext_api::ExtensionUiDialogOptions::default(),
                    None,
                    serde_json::json!({"method":"editor","title":title,"prefill":prefill}),
                    |response| {
                        if response["cancelled"].as_bool() == Some(true) {
                            None
                        } else {
                            response["value"].as_str().map(str::to_owned)
                        }
                    },
                )
                .await)
        })
    }
    fn notify(&self, message: &str, kind: maho_ext_api::NotificationType) {
        let kind = match kind {
            maho_ext_api::NotificationType::Info => "info",
            maho_ext_api::NotificationType::Warning => "warning",
            maho_ext_api::NotificationType::Error => "error",
        };
        (self.out)(
            serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"notify","message":message,"notifyType":kind}),
        );
    }
    fn set_status(&self, key: &str, text: Option<&str>) {
        (self.out)(
            serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setStatus","statusKey":key,"statusText":text}),
        );
    }
    fn set_widget(
        &self,
        key: &str,
        content: Option<maho_ext_api::WidgetContent>,
        options: maho_ext_api::ExtensionWidgetOptions,
    ) {
        let target = RenderedTarget::Widget(options.placement.as_str());
        match content {
            Some(maho_ext_api::WidgetContent::Component(factory)) => {
                self.retain_renderer(key, factory, target)
            }
            Some(maho_ext_api::WidgetContent::Lines(lines)) => {
                self.dispose_renderer(key);
                (self.out)(target.record(key, Some(lines)));
            }
            None => {
                self.dispose_renderer(key);
                (self.out)(target.record(key, None));
            }
        }
    }
    fn set_header(&self, factory: Option<maho_ext_api::ComponentFactory>) {
        match factory {
            Some(factory) => {
                self.retain_renderer(HEADER_RENDERER_KEY, factory, RenderedTarget::Header)
            }
            None => {
                self.dispose_renderer(HEADER_RENDERER_KEY);
                if self.has_rendered_components() {
                    (self.out)(RenderedTarget::Header.record(HEADER_RENDERER_KEY, None));
                }
            }
        }
    }
    fn set_footer(&self, factory: Option<maho_ext_api::ComponentFactory>) {
        match factory {
            Some(factory) => {
                self.retain_renderer(FOOTER_RENDERER_KEY, factory, RenderedTarget::Footer)
            }
            None => {
                self.dispose_renderer(FOOTER_RENDERER_KEY);
                if self.has_rendered_components() {
                    (self.out)(RenderedTarget::Footer.record(FOOTER_RENDERER_KEY, None));
                }
            }
        }
    }
    fn set_title(&self, title: &str) {
        (self.out)(
            serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setTitle","title":title}),
        );
    }
    fn paste_to_editor(&self, text: &str) {
        self.set_editor_text(text);
    }
    fn set_editor_text(&self, text: &str) {
        (self.out)(
            serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"set_editor_text","text":text}),
        );
    }
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _factory: maho_ext_api::ComponentFactory,
        _options: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, serde_json::Value> {
        if let Some(request) = crate::custom_capability::build_custom_unsupported_request(
            &self.capabilities,
            crate::custom_capability::DEFAULT_CUSTOM_EXTENSION_LABEL,
            &uuid::Uuid::new_v4().to_string(),
        ) {
            (self.out)(request);
        }
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
    fn theme(&self) -> maho_ext_api::Theme {
        maho_ext_api::Theme::default()
    }
}

/**
 * Per-connection RPC handler: one session, one output sink, and the JSONL command loop.
 *
 * It owns exactly one session and speaks the protocol over an injected sink; process signals,
 * stdin wiring and exit belong to the host (senpi `createRpcConnectionHandler`).
 */
pub struct RpcConnectionHandler {
    session: AgentSession,
    ui: std::sync::Arc<RpcExtensionUi>,
    pending: std::sync::Arc<
        std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>,
    >,
    questions: std::sync::Arc<
        std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>,
    >,
    shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    active_logins: ActiveLogins,
    loaded_surfaces_subscription: std::sync::Mutex<Option<maho_ext_api::BusSubscription>>,
    loaded_surfaces_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) last_abort_source: LastAbortSource,
    /// senpi `unsubscribeExtensionEvents`: present only for an `extension_events` client.
    extension_events_subscription:
        std::sync::Mutex<Option<maho_core::event_bus::EventSubscription>>,
    /// senpi `setRebindSession`: in-place session replacement and reload notifications.
    rebind_subscription: std::sync::Mutex<Option<maho_core::event_bus::EventSubscription>>,
    /// senpi `unsubscribeProviderAccountEvents`.
    provider_account_unsubscribe: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
    /// senpi `replacementIssuedHere`: replacements this connection's own commands drive.
    local_replacements: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// senpi `options.connectionRoles`: this connection's declared roles (a classic handler serves one connection).
    connection_roles: crate::session_binding::SharedConnectionRoles,
    grant_presentation:
        std::sync::Mutex<Option<maho_core::mcp_grant_presentations::McpGrantAttachment>>,
    relocation: crate::session_relocation_legacy::RpcSessionRelocation,
    /// senpi `options.toolRuntime`: a single-session `--mode rpc` process advertises its `--tool-runtime` as
    /// `tool_runtime:<value>`.
    tool_runtime: Option<crate::host_capabilities::ToolRuntimeKind>,
}
/// senpi connection-handler.ts `get_protocol_info` capability tail: the classic binding's own tool-runtime answer
/// (when it has one) goes ahead of the negotiated client capabilities, and a client flag can never advertise a tool
/// runtime, so a negotiated `tool_runtime:*` / `placement:*` / `offload_api` entry is dropped.
pub fn classic_protocol_capabilities(
    capabilities: &[String],
    tool_runtime: Option<crate::host_capabilities::ToolRuntimeKind>,
    base_len: usize,
) -> Vec<String> {
    let split = base_len.min(capabilities.len());
    let (base, negotiated) = capabilities.split_at(split);
    let mut out: Vec<String> = base.to_vec();
    if let Some(kind) = tool_runtime {
        for capability in crate::host_capabilities::tool_runtime_capabilities(kind) {
            if !out.contains(&capability) {
                out.push(capability);
            }
        }
    }
    for capability in negotiated {
        if !crate::host_capabilities::is_host_owned_tool_runtime_capability(capability)
            && !out.contains(capability)
        {
            out.push(capability.clone());
        }
    }
    out
}
/// senpi `buildRpcCommandsForSession`: the three command groups classified by their real source.
pub fn build_rpc_commands_for_session(
    session: &AgentSession,
) -> Vec<crate::rpc_command_surface::RpcSlashCommand> {
    let (extensions, templates, skills) = session.command_groups();
    crate::rpc_command_surface::build_rpc_commands(
        &crate::rpc_command_surface::command_inputs(&extensions),
        &crate::rpc_command_surface::command_inputs(&templates),
        &crate::rpc_command_surface::command_inputs(&skills),
    )
}
/// The command half of senpi `refreshLoadedSurfacesAfter`: record the new digest and publish
/// `commands_changed` only for a post-baseline change.
fn refresh_rpc_commands(
    session: &AgentSession,
    digest: &std::sync::Mutex<Option<String>>,
    out: &RpcRecordSink,
) {
    let commands = build_rpc_commands_for_session(session);
    let changed = {
        let mut digest = digest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed =
            crate::rpc_command_surface::create_commands_changed_event(digest.as_deref(), &commands);
        if let Ok(next) = crate::rpc_command_surface::rpc_command_list_digest(&commands) {
            *digest = Some(next);
        }
        changed
    };
    match changed {
        Ok(Some(event)) => out(event),
        Ok(None) => {}
        Err(error) => out(serde_json::json!({"type":"rpc_error","error":error.to_string()})),
    }
}
impl RpcConnectionHandler {
    /// Build a handler over `session` and install the RPC extension UI on it.
    pub async fn create(
        session: AgentSession,
        out: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
        capabilities: Vec<String>,
    ) -> Self {
        Self::create_with_shutdown_handler(session, out, capabilities, None).await
    }
    /// senpi `options.shutdownHandler`: an extension's `ctx.shutdown()` calls `shutdown_handler` when
    /// given, otherwise it only marks this connection for the host's shutdown check.
    pub async fn create_with_shutdown_handler(
        session: AgentSession,
        out: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
        capabilities: Vec<String>,
        shutdown_handler: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let pending: std::sync::Arc<
            std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>,
        > = Default::default();
        let questions: std::sync::Arc<
            std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>,
        > = Default::default();
        let extension_events = capabilities
            .iter()
            .any(|capability| capability == crate::custom_capability::EXTENSION_EVENTS_CAPABILITY);
        let ui = std::sync::Arc::new(RpcExtensionUi::new(
            out.clone(),
            pending.clone(),
            questions.clone(),
            capabilities,
        ));
        let shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
        let account_out = out.clone();
        let provider_account_unsubscribe = maho_core::subscribe_provider_account_events(
            std::sync::Arc::new(move |event: maho_core::ProviderAccountEvent| {
                account_out(match event {
                    maho_core::ProviderAccountEvent::AccountsChanged { provider } => {
                        serde_json::json!({"type":"auth_accounts_changed","provider":provider})
                    }
                    maho_core::ProviderAccountEvent::Failover {
                        provider,
                        from,
                        to,
                        reason,
                    } => {
                        serde_json::json!({"type":"account_failover","provider":provider,"from":from,"to":to,"reason":reason})
                    }
                });
            }),
        );
        let extension_events_subscription=extension_events.then(||{
            let event_out=out.clone();
            session.on_extension_event(maho_core::EXTENSION_RPC_EVENT_CHANNEL,std::sync::Arc::new(move|data:&serde_json::Value|{
                event_out(serde_json::json!({"type":"extension_event","name":data["name"],"data":data["data"]}));
            }))
        });
        let local_replacements: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
        let commands_digest: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
        let rebind_session = session.weak_accessor();
        let rebind_out = out.clone();
        let rebind_digest = commands_digest.clone();
        let rebind_local = local_replacements.clone();
        let rebind_subscription = session.on_extension_event(
            maho_core::agent_session::SESSION_REBIND_CHANNEL,
            std::sync::Arc::new(move |data: &serde_json::Value| {
                let Some(session) = rebind_session() else {
                    return;
                };
                // A replacement driven by this connection's own command is acknowledged by its response.
                if data["reason"] != "reload"
                    && rebind_local.load(std::sync::atomic::Ordering::SeqCst) == 0
                {
                    let event = crate::rpc_types::RpcSessionLifecycleEvent::SessionReplaced {
                        durable_session_id: session.session_id(),
                        session_file: session.session_file(),
                        cwd: session.with_session_manager(|manager| manager.cwd().to_owned()),
                        session_name: session.session_name(),
                    };
                    match serde_json::to_value(event) {
                        Ok(record) => rebind_out(record),
                        Err(error) => rebind_out(
                            serde_json::json!({"type":"rpc_error","error":error.to_string()}),
                        ),
                    }
                }
                refresh_rpc_commands(&session, &rebind_digest, &rebind_out);
            }),
        );
        let sink = out.clone();
        let shutdown_flag = shutdown_requested.clone();
        let bindings = maho_core::agent_session::ExtensionBindings {
            ui_context: Some(ui.clone()),
            mode: Some(maho_ext_api::ExtensionMode::Rpc),
            on_error: Some(std::sync::Arc::new(
                move |error: &maho_ext_api::ExtensionError| {
                    (sink)(
                        serde_json::json!({"type":"extension_error","extensionPath":error.extension_path,"event":error.event,"error":error.error}),
                    );
                },
            )),
            shutdown_handler: Some(std::sync::Arc::new(move || match &shutdown_handler {
                Some(handler) => handler(),
                None => shutdown_flag.store(true, std::sync::atomic::Ordering::SeqCst),
            })),
            ..Default::default()
        };
        // The CLI mount already starts host sessions; attach only replaces the connection context.
        if session.has_started_extension_lifecycle() {
            session.rebind_extension_context(bindings).await;
        } else {
            session.bind_extensions(bindings).await;
        }
        // The first snapshot is the baseline and is never published.
        refresh_rpc_commands(&session, &commands_digest, &out);
        let inventory_session = session.weak_accessor();
        let inventory_out = out.clone();
        let (inventory_tx, mut inventory_rx) =
            tokio::sync::mpsc::unbounded_channel::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>();
        let loaded_surfaces_subscription = session
            .bind_mcp_wire_status::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>(
                std::sync::Arc::new(move |snapshot| {
                    let _ = inventory_tx.send(snapshot.clone());
                }),
            )
            .await;
        let loaded_surfaces_task = tokio::spawn(async move {
            let mut fingerprint = None::<serde_json::Value>;
            while let Some(snapshot) = inventory_rx.recv().await {
                let Some(session) = inventory_session() else {
                    break;
                };
                let extensions = session.loaded_extension_surfaces().await;
                let mcp_servers = snapshot.servers.iter().map(|server| {
                    let status = server.status.as_ref().map(|status|serde_json::json!(status)).unwrap_or_else(||serde_json::json!(if server.server_info.is_some() { "connected" } else if server.auth_status == maho_ext_api::mcp_wire::McpWireAuthStatus::NotLoggedIn { "needs_auth" } else { "enabled" }));
                    serde_json::json!({"name":server.name,"toolCount":server.tools.len(),"status":status,"authStatus":server.auth_status})
                }).collect::<Vec<_>>();
                let value = serde_json::json!({"extensions":extensions,"mcpServers":mcp_servers});
                let skills = session.loaded_skills().unwrap_or_default().skills.into_iter().map(|skill| serde_json::json!({"name":skill.name,"path":skill.file_path,"sourceInfo":skill.source_info,"enabled":!skill.disable_model_invocation})).collect::<Vec<_>>();
                let current = serde_json::json!({"skills":skills,"extensions":value["extensions"],"mcpServers":value["mcpServers"]});
                let changed = fingerprint.as_ref().is_some_and(|prior| prior != &current);
                fingerprint = Some(current);
                if changed {
                    inventory_out(serde_json::json!({"type":"loaded_surfaces_changed"}));
                }
            }
        });
        let grant_ui = ui.clone();
        let grant_presentation = ui
            .capabilities
            .iter()
            .any(|capability| capability == "grant_select")
            .then(|| {
                maho_core::mcp_grant_presentations::attach_mcp_grant_presentation(
                    &session.session_id(),
                    std::sync::Arc::new(move |title, choices, _signal| {
                        let answer = grant_ui.select_grant(&title, &choices, &Default::default());
                        Box::pin(async move { Ok(answer.await) })
                    }),
                )
            });
        Self {
            grant_presentation: std::sync::Mutex::new(grant_presentation),
            session,
            ui,
            pending,
            questions,
            shutdown_requested,
            active_logins: Default::default(),
            loaded_surfaces_subscription: std::sync::Mutex::new(loaded_surfaces_subscription),
            loaded_surfaces_task: tokio::sync::Mutex::new(Some(loaded_surfaces_task)),
            extension_events_subscription: std::sync::Mutex::new(extension_events_subscription),
            rebind_subscription: std::sync::Mutex::new(Some(rebind_subscription)),
            provider_account_unsubscribe: std::sync::Mutex::new(Some(provider_account_unsubscribe)),
            local_replacements,
            last_abort_source: Default::default(),
            relocation: Default::default(),
            connection_roles: Default::default(),
            tool_runtime: None,
        }
    }
    /// senpi `options.toolRuntime`: advertise this process's `--tool-runtime` in `get_protocol_info`.
    pub fn with_tool_runtime(
        mut self,
        tool_runtime: Option<crate::host_capabilities::ToolRuntimeKind>,
    ) -> Self {
        self.tool_runtime = tool_runtime;
        self
    }
    pub fn with_connection_roles(
        mut self,
        roles: crate::session_binding::SharedConnectionRoles,
    ) -> Self {
        self.connection_roles = roles;
        self
    }
    pub fn ui(&self) -> &std::sync::Arc<RpcExtensionUi> {
        &self.ui
    }
    /// Feed one inbound JSONL line: a UI response settles a pending dialog; anything else is a command.
    pub async fn handle_input_line(&self, line: &str) -> Result<Option<String>, serde_json::Error> {
        self.handle_input_line_for_connection(line, None).await
    }
    pub async fn handle_input_line_for_connection(
        &self,
        line: &str,
        connection: Option<&str>,
    ) -> Result<Option<String>, serde_json::Error> {
        let parsed = serde_json::from_str::<serde_json::Value>(line).ok();
        if let Some(value) = &parsed
            && matches!(
                value["type"].as_str(),
                Some("extension_ui_response" | "extension_ui_progress")
            )
        {
            let mut answer = value.clone();
            if let Some(id) = crate::extension_ui_response::answered_ui_request_id(value) {
                answer["id"] = id.into();
            }
            let value = &answer;
            let capable = connection.is_some()
                || self
                    .ui
                    .capabilities
                    .iter()
                    .any(|capability| capability == "grant_select");
            if !self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .grant_authorized(
                    value["id"].as_str().unwrap_or_default(),
                    connection,
                    capable,
                )
            {
                return Ok(None);
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64);
            let (kind, resolved) = self
                .questions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .respond(value, now);
            let error = match kind {
                crate::connection_question_bridge::QuestionReply::Incomplete => {
                    Some("question_incomplete")
                }
                crate::connection_question_bridge::QuestionReply::AlreadyResolved => {
                    Some("question_already_resolved")
                }
                crate::connection_question_bridge::QuestionReply::Unknown => {
                    let matched = self
                        .pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .resolve(value.clone());
                    if !matched && connection.is_some() {
                        Some("unknown_extension_ui_request")
                    } else {
                        None
                    }
                }
                crate::connection_question_bridge::QuestionReply::Accepted => None,
            };
            if let Some(record) = resolved {
                self.out_record(record);
            }
            if error.is_some()
                || kind == crate::connection_question_bridge::QuestionReply::Accepted
                || connection.is_some()
            {
                let mut reply = serde_json::json!({"id":parsed.as_ref().map(|frame|frame["id"].clone()),"type":"response","command":"extension_ui_response","success":error.is_none()});
                if let Some(error) = error {
                    reply["error"] = error.into();
                }
                return crate::jsonl::serialize_json_line(&reply).map(Some);
            }
            return Ok(None);
        }
        let kind = parsed.as_ref().and_then(|value| value["type"].as_str());
        if kind == Some("host_response")
            && let Some(frame) = &parsed
        {
            let outcome = self.relocation.respond(frame, connection.unwrap_or("sole"));
            return crate::jsonl::serialize_json_line(
                &crate::session_relocation_channel::answer_host_response(
                    frame,
                    Some(&mut |_| outcome),
                ),
            )
            .map(Some);
        }
        // senpi `set_client_info`: a connection's declared role is fixed once declared.
        if kind == Some("set_client_info")
            && let Some(value) = &parsed
            && let Some(capabilities) = value["capabilities"].as_array()
        {
            let capabilities = capabilities
                .iter()
                .filter_map(|capability| capability.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            if !self
                .connection_roles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .declare(
                    crate::connection_roles::role_connection(connection),
                    &capabilities,
                )
            {
                let response = RpcResponse {
                    id: value["id"].as_str().map(str::to_owned),
                    record_type: ResponseRecordType::Response,
                    command: "set_client_info".into(),
                    session_id: value["sessionId"].as_str().map(str::to_owned),
                    result: RpcResponseResult::Error {
                        error: crate::connection_roles::CONNECTION_ROLE_FIXED.into(),
                        error_code: None,
                        error_data: None,
                    },
                };
                return crate::jsonl::serialize_json_line(&response).map(Some);
            }
        }
        if kind == Some("set_client_info")
            && connection.is_none()
            && let Some(capabilities) = parsed
                .as_ref()
                .and_then(|value| value["capabilities"].as_array())
        {
            let mut presentation = self
                .grant_presentation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if capabilities
                .iter()
                .any(|capability| capability == "grant_select")
            {
                if presentation.is_none() {
                    let ui = self.ui.clone();
                    *presentation = Some(
                        maho_core::mcp_grant_presentations::attach_mcp_grant_presentation(
                            &self.session.session_id(),
                            std::sync::Arc::new(move |title, choices, _signal| {
                                let answer = ui.select_grant(&title, &choices, &Default::default());
                                Box::pin(async move { Ok(answer.await) })
                            }),
                        ),
                    );
                }
            } else if presentation.take().is_some() {
                self.pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .cancel_grants_for_classic();
            }
        }
        // senpi `set_client_info`: live components re-render at the client's width.
        if kind == Some("set_client_info")
            && let Some(width) = parsed
                .as_ref()
                .and_then(|value| value["width"].as_f64())
                .filter(|width| width.is_finite() && *width > 0.)
        {
            self.ui.set_client_width(width as usize);
            self.ui.rerender_live_components();
        }
        // These commands are role-sensitive and deliberately handled before the general command enum.
        if let Some(value) = &parsed {
            let interactive = self
                .connection_roles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .has(
                    connection,
                    crate::custom_capability::INTERACTIVE_USER_CAPABILITY,
                );
            let command_kind = kind.unwrap_or_default();
            let task_parent = self
                .connection_roles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .has(connection, crate::custom_capability::TASK_PARENT_CAPABILITY);
            let result = if let Some(error) =
                crate::pause_command_validation::validate_pause_command(value)
            {
                Some(Err(error.to_owned()))
            } else {
                match command_kind {
                    "resume_session" => Some(self.session.begin_resume(value).await),
                    "hold_input" => Some(crate::held_input_commands::run_hold_input(
                        &self.session,
                        value,
                        interactive,
                        task_parent,
                    )),
                    "withdraw_held_input" => Some(if interactive {
                        self.session
                            .withdraw_held_input(value["inputId"].as_str().unwrap_or_default())
                            .map(|withdrawn| serde_json::json!({"withdrawn":withdrawn}))
                    } else {
                        Err("withdraw_held_input_requires_interactive_client".into())
                    }),
                    "pause_for_run" => Some(
                        crate::run_hold_commands::run_pause_for_run(
                            &self.session,
                            value,
                            task_parent,
                        )
                        .await,
                    ),
                    "resume_for_run" => Some(
                        crate::run_hold_commands::run_resume_for_run(
                            &self.session,
                            value,
                            task_parent,
                        )
                        .await,
                    ),
                    "prompt" | "steer" | "follow_up"
                        if value["message"].is_string()
                            && crate::automatic_input_gate::hold_automatic_input(
                                &self.session,
                                value["message"].as_str().unwrap_or_default(),
                                interactive,
                            ) =>
                    {
                        Some(Ok(
                            serde_json::json!({"disposition":"handled","heldAsNotice":true}),
                        ))
                    }
                    _ => None,
                }
            };
            if let Some(result) = result {
                let response = RpcResponse {
                    id: value["id"].as_str().map(str::to_owned),
                    record_type: ResponseRecordType::Response,
                    command: command_kind.into(),
                    session_id: value["sessionId"].as_str().map(str::to_owned),
                    result: match result {
                        Ok(data) => RpcResponseResult::Success { data: Some(data) },
                        Err(error) => RpcResponseResult::Error {
                            error,
                            error_code: None,
                            error_data: None,
                        },
                    },
                };
                return crate::jsonl::serialize_json_line(&response).map(Some);
            }
            if matches!(command_kind, "abort" | "interrupt") && value["intent"] == "stop" {
                if !interactive {
                    return crate::jsonl::serialize_json_line(&serde_json::json!({"id":value["id"],"type":"response","command":command_kind,"success":false,"error":"stop_requires_interactive_client: declare interactive_user with set_client_info first"})).map(Some);
                }
                self.session.stop_for_run();
            }
        }
        let replaces = matches!(
            kind,
            Some("new_session" | "switch_session" | "fork" | "clone" | "import_jsonl")
        );
        if replaces {
            self.local_replacements
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let result = handle_input_line_with_login_state(
            &self.session,
            line,
            Some(self.ui.out.clone()),
            Some(self.active_logins.clone()),
            Some(self.ui.clone()),
            Some(&self.last_abort_source),
        )
        .await;
        if replaces {
            self.local_replacements
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
        if kind == Some("get_state")
            && let Ok(Some(line)) = &result
            && let Ok(mut record) = serde_json::from_str::<serde_json::Value>(line)
            && record["success"] == true
            && record["data"].is_object()
        {
            // senpi `pendingDialogs`: the live dialog frames belong to this connection's request store.
            record["data"]["pendingDialogs"] = self.ui.pending_dialogs().into();
            return crate::jsonl::serialize_json_line(&record).map(Some);
        }
        if kind == Some("get_protocol_info")
            && let Ok(Some(line)) = &result
            && let Ok(mut record) = serde_json::from_str::<serde_json::Value>(line)
            && record["success"] == true
            && let Some(capabilities) = record["data"]["capabilities"].as_array()
        {
            let capabilities = capabilities
                .iter()
                .filter_map(|capability| capability.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            record["data"]["capabilities"] = serde_json::json!(classic_protocol_capabilities(
                &capabilities,
                self.tool_runtime,
                CLASSIC_BASE_CAPABILITIES,
            ));
            return crate::jsonl::serialize_json_line(&record).map(Some);
        }
        result
    }
    fn out_record(&self, record: serde_json::Value) {
        (self.ui.out)(record);
    }
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    /// senpi `rerenderComponents`.
    pub fn rerender_components(&self) {
        self.ui.rerender_components();
    }
    /// Cancel UI requests that can no longer be answered by this connection.
    pub fn cancel_pending_extension_ui_requests(&self) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_all();
        self.questions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_all(0);
    }
    /// Tear down this connection's subscriptions and renderers while leaving the session alive
    /// (senpi `dispose` with `disposeRuntime: false`).
    pub fn cancel_grant(&self, id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resolve(serde_json::json!({"id":id,"cancelled":true}));
    }
    pub fn bind_grant_connection(&self, id: &str, connection: &str) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bind_grant(id, Some(connection.into()));
    }
    pub fn register_grant_presentation(&self, session: &str) {
        let ui = self.ui.clone();
        *self
            .grant_presentation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
            maho_core::mcp_grant_presentations::attach_mcp_grant_presentation(
                session,
                std::sync::Arc::new(move |title, choices, _signal| {
                    let answer = ui.select_grant(&title, &choices, &Default::default());
                    Box::pin(async move { Ok(answer.await) })
                }),
            ),
        );
    }
    pub fn relocation(&self) -> &crate::session_relocation_legacy::RpcSessionRelocation {
        &self.relocation
    }
    pub async fn relocation_targets(&self) -> serde_json::Value {
        let offered = self
            .ui
            .capabilities
            .iter()
            .any(|cap| cap == "session_relocation");
        self.relocation
            .targets(offered.then_some("sole"), |record| self.out_record(record))
            .await
    }
    pub async fn request_relocation(&self, target: &str, reason: &str) -> serde_json::Value {
        let offered = self
            .ui
            .capabilities
            .iter()
            .any(|cap| cap == "session_relocation");
        self.relocation
            .request(
                target,
                reason,
                offered.then_some("sole"),
                self.session.session_file().is_some(),
                |record| self.out_record(record),
            )
            .await
    }
    pub fn connection_detached(&self, connection: &str) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel_grants_for(connection);
        if let Some(record) = self.relocation.connection_detached(connection) {
            self.session.with_session_manager_mut(|manager| {
                manager.append_custom_message(
                    "session_relocation",
                    record["content"].clone(),
                    false,
                    Some(record["details"].clone()),
                )
            });
        }
    }
    pub async fn release(&self) {
        self.grant_presentation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.relocation.dispose();
        self.ui.dispose_all_renderers();
        self.cancel_pending_extension_ui_requests();
        let unsubscribe = self
            .provider_account_unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.extension_events_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.rebind_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.loaded_surfaces_subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = self.loaded_surfaces_task.lock().await.take() {
            task.abort();
            if let Err(error) = task.await
                && !error.is_cancelled()
            {
                eprintln!("RPC inventory consumer failed: {error}");
            }
        }
    }
    /// Tear down subscriptions and dispose the session runtime. Never exits the process.
    pub async fn dispose(&self) {
        self.dispose_with(false).await;
    }
    /// senpi `dispose({ processExit })`: `process_exit` marks a caller that exits right after, so quit-time handlers
    /// would share one total budget; the session hook that arms it is not exposed by `AgentSession` yet.
    pub async fn dispose_with(&self, process_exit: bool) {
        let _ = process_exit;
        self.release().await;
        self.session
            .emit_session_shutdown(maho_ext_api::SessionReason::Quit)
            .await;
        self.session.dispose().await;
    }
}

#[cfg(test)]
mod new_session_tests {
    use super::*;
    use std::sync::Arc;

    fn session() -> AgentSession {
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                providers: Some(vec![]),
                ..Default::default()
            },
        )
        .expect("model runtime");
        AgentSession::new(maho_core::agent_session::AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                stream_fn: Some(Arc::new(|_, _, _| {
                    maho_ai::types::AssistantMessageEventStream::assistant()
                })),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                "/tmp", None, None,
            ),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".into(),
            agent_dir: Some("/tmp/maho-agent".into()),
            fallback_now: None,
            retry_random: None,
            scoped_models: vec![],
            favorite_models: vec![],
            flag_values: Default::default(),
            custom_tools: vec![],
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: Some(false),
        })
        .expect("session")
    }

    #[tokio::test]
    async fn switch_to_a_session_whose_cwd_is_gone_carries_the_missing_cwd_error_data() {
        let session = session();
        let dir = tempfile::tempdir().expect("directory");
        let gone = dir.path().join("gone").to_string_lossy().into_owned();
        let path = dir.path().join("stored.jsonl");
        std::fs::write(&path,format!("{}\n",serde_json::json!({"type":"session","version":3,"id":"missing-cwd","timestamp":"2026-01-01T00:00:00Z","cwd":gone}))).expect("session header");
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":"switch","type":"switch_session","sessionPath":path.to_string_lossy()})).expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["success"], false);
        assert_eq!(response["errorCode"], "missing_session_cwd");
        assert_eq!(response["errorData"]["sessionCwd"], gone);
        assert_eq!(response["errorData"]["fallbackCwd"], "/tmp");
        assert_eq!(session.cwd(), "/tmp");
    }

    #[tokio::test]
    async fn new_session_without_parent_returns_navigation_result() {
        let session = session();
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"id":"new","type":"new_session"}))
                .expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "new_session");
        assert_eq!(response["success"], true);
        assert_eq!(response["data"], serde_json::json!({"cancelled":false}));
    }

    #[tokio::test]
    async fn new_session_with_parent_preserves_success_shape() {
        let session = session();
        let command: RpcCommand = serde_json::from_value(
            serde_json::json!({"type":"new_session","parentSession":"parent-session"}),
        )
        .expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "new_session");
        assert_eq!(response["success"], true);
        assert_eq!(response["data"], serde_json::json!({"cancelled":false}));
    }

    #[tokio::test]
    async fn fork_preserves_response_shape_and_error_semantics() {
        let session = session();
        let command: RpcCommand = serde_json::from_value(
            serde_json::json!({"id":"fork","type":"fork","entryId":"missing"}),
        )
        .expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "fork");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Invalid entry ID for forking");
    }

    #[tokio::test]
    async fn clone_without_leaf_returns_exact_error() {
        let session = session();
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"id":"clone","type":"clone"}))
                .expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "clone");
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"],
            "Cannot clone session: no current entry selected"
        );
    }

    #[tokio::test]
    async fn malformed_command_fields_return_error_response() {
        let response = handle_input_line(&session(), r#"{"id":"m","type":"set_model"}"#)
            .await
            .expect("dispatch")
            .expect("response");
        let response: serde_json::Value = serde_json::from_str(&response).expect("wire");
        assert_eq!(response["id"], "m");
        assert_eq!(response["command"], "set_model");
        assert_eq!(response["success"], false);
        assert!(response["error"].is_string());
    }

    #[tokio::test]
    async fn classic_protocol_info_and_multi_session_commands() {
        let session = session();
        let info = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(serde_json::json!({"id":"p","type":"get_protocol_info"}))
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(info["success"], true);
        assert_eq!(info["data"]["mode"], "classic");
        assert_eq!(info["data"]["protocolVersion"], 1);
        assert!(
            info["data"]["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|capability| capability == "multi_session")
        );
        let open = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(serde_json::json!({"type":"open_session"})).unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(open["success"], false);
        assert_eq!(open["error"], "multi_session_disabled");
        let close = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(serde_json::json!({"type":"close_session"})).unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(close["error"], "Unknown command: close_session");
    }

    #[tokio::test]
    async fn navigate_tree_requires_one_target() {
        let session = session();
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"type":"navigate_tree"})).expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(
            response["error"],
            "navigate_tree requires entryId or targetId"
        );
    }

    #[tokio::test]
    async fn navigate_tree_reports_coded_refusals_and_a_boolean_cancelled() {
        let session = session();
        let root = session.with_session_manager_mut(|manager| manager.append_custom("test", None));
        let root_id = root["id"].as_str().expect("root").to_owned();
        let send = |value: serde_json::Value| {
            let session = session.clone();
            async move {
                let command: RpcCommand = serde_json::from_value(value).expect("command");
                serde_json::to_value(
                    handle_session_command(&session, &command)
                        .await
                        .expect("response"),
                )
                .expect("wire")
            }
        };
        let stale=send(serde_json::json!({"type":"navigate_tree","targetId":"absent","expectedLeafId":"older"})).await;
        assert_eq!(stale["errorCode"], "stale_leaf");
        assert_eq!(
            stale["error"],
            format!("Session leaf moved (expected older, now {root_id})")
        );
        let missing = send(serde_json::json!({"type":"navigate_tree","targetId":"absent"})).await;
        assert_eq!(missing["errorCode"], "not_found");
        let navigated = send(serde_json::json!({"type":"navigate_tree","targetId":root_id})).await;
        assert_eq!(navigated["data"]["cancelled"], false);
    }

    #[tokio::test]
    async fn get_tree_returns_tree_and_leaf_id() {
        let session = session();
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"type":"get_tree"})).expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "get_tree");
        assert!(response["data"].get("tree").is_some());
        assert!(response["data"].get("leafId").is_some());
    }

    #[tokio::test]
    async fn import_jsonl_reports_missing_file() {
        let session = session();
        let target = if cfg!(windows) {
            "C:\\tmp\\does-not-exist-4010.jsonl"
        } else {
            "/tmp/does-not-exist-4010.jsonl"
        };
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"type":"import_jsonl","inputPath":target}))
                .expect("command");
        let response = serde_json::to_value(
            handle_session_command(&session, &command)
                .await
                .expect("response"),
        )
        .expect("wire");
        assert_eq!(response["command"], "import_jsonl");
        assert_eq!(response["error"], format!("File not found: {target}"));
    }

    #[tokio::test]
    async fn append_user_message_dispatches() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"append_user_message","content":"hello"}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "append_user_message");
        assert_eq!(response["success"], true);
    }
    #[tokio::test]
    async fn get_media_reports_missing_media() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"get_media","toolCallId":"missing","contentIndex":0}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "get_media");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "media_not_found");
    }
    #[tokio::test]
    async fn get_media_reads_durable_session_entries() {
        let session = session();
        session.with_session_manager_mut(|manager|{manager.append_message(serde_json::json!({"role":"toolResult","toolCallId":"durable","toolName":"read","content":[{"type":"text","text":"x"},{"type":"image","data":"AAAA","mimeType":"image/png"}],"isError":false,"timestamp":0}));});
        let response = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(
                    serde_json::json!({"type":"get_media","toolCallId":"durable","contentIndex":1}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["content"]["data"], "AAAA");
    }
    #[tokio::test]
    async fn routed_bash_brackets_with_start_and_end_events() {
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = records.clone();
        let sink: RpcRecordSink =
            std::sync::Arc::new(move |record| observed.lock().unwrap().push(record));
        let command: RpcCommand = serde_json::from_value(
            serde_json::json!({"type":"bash","command":"printf rpc","sessionId":"route"}),
        )
        .unwrap();
        let response = serde_json::to_value(
            handle_session_command_with_sink(&session(), &command, Some(sink), None, None, None)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["success"], true);
        let kinds = records
            .lock()
            .unwrap()
            .iter()
            .filter_map(|record| record["type"].as_str().map(str::to_owned))
            .filter(|kind| kind.starts_with("bash_") && kind != "bash_execution_update")
            .collect::<Vec<_>>();
        assert_eq!(kinds, ["bash_start", "bash_end"]);
    }
    #[tokio::test]
    async fn agent_end_records_abort_source_for_get_state_and_omits_aborted_when_false() {
        let session = session();
        let last: LastAbortSource = Default::default();
        let clean = rpc_session_event_record(
            &session,
            &maho_ext_api::AgentSessionEvent::AgentEnd {
                messages: vec![],
                aborted: false,
                will_retry: false,
                abort_source: None,
            },
            &last,
        )
        .unwrap();
        assert!(clean.get("aborted").is_none());
        let aborted = rpc_session_event_record(
            &session,
            &maho_ext_api::AgentSessionEvent::AgentEnd {
                messages: vec![],
                aborted: true,
                will_retry: false,
                abort_source: Some(maho_ext_api::AbortSource::User),
            },
            &last,
        )
        .unwrap();
        assert_eq!(aborted["aborted"], true);
        assert_eq!(aborted["abortSource"], "user");
        let command: RpcCommand =
            serde_json::from_value(serde_json::json!({"type":"get_state"})).unwrap();
        let state = serde_json::to_value(
            handle_session_command_with_sink(&session, &command, None, None, None, Some(&last))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state["data"]["lastAbortSource"], "user");
    }
    #[tokio::test]
    async fn get_loaded_surfaces_dispatches_inventory() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"get_loaded_surfaces"})).unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "get_loaded_surfaces");
        assert_eq!(response["success"], true);
        assert!(response["data"]["extensions"].is_array());
        assert!(response["data"]["mcpServers"].is_array());
    }
    #[tokio::test]
    async fn extension_request_rejects_empty_name() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"extension_request","name":"   "}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "extension_request");
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"],
            "Extension RPC request name cannot be empty"
        );
    }
    #[tokio::test]
    async fn get_auth_providers_dispatches_provider_statuses() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"get_auth_providers"})).unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "get_auth_providers");
        assert_eq!(response["success"], true);
        assert!(response["data"]["providers"].is_array());
    }
    #[tokio::test]
    async fn login_api_key_persists_api_key_credential() {
        let session = session();
        let response = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(
                    serde_json::json!({"type":"login_api_key","provider":"fixture","key":"secret"}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "login_api_key");
        assert_eq!(response["success"], true);
        assert!(response.get("data").is_none());
        assert_eq!(
            session.model_registry().auth_storage.get("fixture"),
            Some(serde_json::json!({"type":"api_key","key":"secret"}))
        );
    }
    #[tokio::test]
    async fn logout_removes_stored_credential() {
        let session = session();
        session
            .model_registry()
            .auth_storage
            .set(
                "fixture",
                Some(serde_json::json!({"type":"api_key","key":"secret"})),
            )
            .unwrap();
        let response = serde_json::to_value(
            handle_session_command(
                &session,
                &serde_json::from_value(serde_json::json!({"type":"logout","provider":"fixture"}))
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "logout");
        assert_eq!(response["success"], true);
        assert!(response.get("data").is_none());
        assert_eq!(session.model_registry().auth_storage.get("fixture"), None);
    }
    #[tokio::test]
    async fn bash_dispatches_success() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"bash","command":"printf rpc"}))
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "bash");
        assert_eq!(response["success"], true);
    }
    #[tokio::test]
    async fn abort_branch_summary_dispatches() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"abort_branch_summary"}))
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "abort_branch_summary");
        assert_eq!(response["success"], true);
    }
    #[tokio::test]
    async fn check_reload_veto_dispatches() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"check_reload_veto"})).unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "check_reload_veto");
        assert_eq!(response["success"], true);
        assert_eq!(response["data"], serde_json::json!({"cancelled":false}));
    }
    #[tokio::test]
    async fn set_fast_mode_without_extension_returns_command_error() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(serde_json::json!({"type":"set_fast_mode","enabled":true}))
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "set_fast_mode");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Extension runner is not bound");
    }
    #[tokio::test]
    async fn send_custom_message_dispatches() {
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"send_custom_message","customType":"notice","content":[{"type":"text","text":"hello"}],"display":true})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"], "send_custom_message");
        assert_eq!(response["success"], true);
    }
    #[tokio::test]
    async fn edit_user_message_reports_exact_missing_entry_error() {
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"edit_user_message","entryId":"missing","text":"hello"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Entry missing not found");
        assert_eq!(response["errorData"], serde_json::json!({"leafId":null}));
    }
    #[tokio::test]
    async fn switch_session_to_missing_cwd_reports_missing_session_cwd() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("stale.jsonl");
        let missing = directory.path().join("gone").to_string_lossy().into_owned();
        std::fs::write(&path,format!("{}\n",serde_json::json!({"type":"session","version":3,"id":"stale","timestamp":"2026-10-13T00:00:00.000Z","cwd":missing}))).expect("session file");
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"switch_session","sessionPath":path}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["success"], false);
        assert_eq!(response["errorCode"], "missing_session_cwd");
        assert_eq!(response["errorData"]["sessionCwd"], missing.as_str());
        assert_eq!(response["errorData"]["fallbackCwd"], "/tmp");
    }
    #[tokio::test]
    async fn edit_assistant_message_reports_exact_missing_entry_error() {
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"edit_assistant_message","entryId":"missing","text":"hello"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Entry missing not found");
    }
    #[tokio::test]
    async fn get_provider_accounts_dispatches_empty_accounts() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"get_provider_accounts","provider":"openai"}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "get_provider_accounts");
        assert_eq!(response["success"], true);
        assert_eq!(response["data"], serde_json::json!({"accounts":[]}));
    }
    #[tokio::test]
    async fn account_pin_preserves_provider_error() {
        let response = serde_json::to_value(
            handle_session_command(
                &session(),
                &serde_json::from_value(
                    serde_json::json!({"type":"account_pin","provider":"openai","name":"missing"}),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response["command"], "account_pin");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Provider account not found: missing");
    }
    #[tokio::test]
    async fn account_remove_preserves_provider_error() {
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"account_remove","provider":"openai","name":"missing"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"], "account_remove");
        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Provider account not found: missing");
    }
}

#[cfg(test)]
mod connection_events_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn session() -> AgentSession {
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                providers: Some(vec![]),
                ..Default::default()
            },
        )
        .expect("model runtime");
        AgentSession::new(maho_core::agent_session::AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                stream_fn: Some(Arc::new(|_, _, _| {
                    maho_ai::types::AssistantMessageEventStream::assistant()
                })),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                "/tmp", None, None,
            ),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".into(),
            agent_dir: Some("/tmp/maho-agent".into()),
            fallback_now: None,
            retry_random: None,
            scoped_models: vec![],
            favorite_models: vec![],
            flag_values: Default::default(),
            custom_tools: vec![],
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: Some(false),
        })
        .expect("session")
    }
    async fn handler(
        session: &AgentSession,
        capabilities: &[&str],
    ) -> (RpcConnectionHandler, Arc<Mutex<Vec<serde_json::Value>>>) {
        let records = Arc::new(Mutex::new(Vec::new()));
        let captured = records.clone();
        let handler = RpcConnectionHandler::create(
            session.clone(),
            Arc::new(move |record| captured.lock().expect("records").push(record)),
            capabilities
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        )
        .await;
        (handler, records)
    }
    fn of_type(records: &Arc<Mutex<Vec<serde_json::Value>>>, kind: &str) -> Vec<serde_json::Value> {
        records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| record["type"] == kind)
            .cloned()
            .collect()
    }
    struct Lines(Vec<String>);
    impl maho_tui::tui::Component for Lines {
        fn render(&mut self, width: usize) -> Vec<String> {
            self.0
                .iter()
                .map(|line| format!("{line}@{width}"))
                .collect()
        }
    }
    fn factory() -> maho_ext_api::ComponentFactory {
        Arc::new(
            |_: &maho_ext_api::Theme| -> Box<dyn maho_tui::tui::Component> {
                Box::new(Lines(vec!["row".into()]))
            },
        )
    }

    #[tokio::test]
    async fn command_stream_routes_ui_responses_to_the_handler() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let session = session();
        let (mut client, host) = tokio::io::duplex(4096);
        let (input, output) = tokio::io::split(host);
        let run = crate::rpc_mode::run_command_stream(&session, input, output);
        let drive = async {
            client.write_all(b"{\"type\":\"extension_ui_response\",\"id\":\"unknown\",\"cancelled\":true}\n{\"id\":\"after\",\"type\":\"get_messages\"}\n").await.expect("write");
            client.shutdown().await.expect("close input");
            let mut received = String::new();
            client.read_to_string(&mut received).await.expect("read");
            received
        };
        let (result, received) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(run, drive)
        })
        .await
        .expect("bounded stream");
        result.expect("stream");
        let records = received
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("record"))
            .collect::<Vec<_>>();
        assert!(
            records
                .iter()
                .all(|record| record["command"] != "extension_ui_response")
        );
        let responses = records
            .iter()
            .filter(|record| record["type"] == "response")
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], "after");
        assert_eq!(responses[0]["success"], true);
    }

    #[tokio::test]
    async fn extension_rpc_events_reach_only_extension_events_clients() {
        let session = session();
        let (capable, capable_records) = handler(
            &session,
            &[crate::custom_capability::EXTENSION_EVENTS_CAPABILITY],
        )
        .await;
        let (plain, plain_records) = handler(&session, &[]).await;
        session.emit_extension_event(
            maho_core::EXTENSION_RPC_EVENT_CHANNEL,
            &serde_json::json!({"name":"omo.memory.updated","data":{"n":1}}),
        );
        assert_eq!(
            of_type(&capable_records, "extension_event"),
            vec![
                serde_json::json!({"type":"extension_event","name":"omo.memory.updated","data":{"n":1}})
            ]
        );
        assert!(of_type(&plain_records, "extension_event").is_empty());
        capable.release().await;
        session.emit_extension_event(
            maho_core::EXTENSION_RPC_EVENT_CHANNEL,
            &serde_json::json!({"name":"later","data":null}),
        );
        assert_eq!(of_type(&capable_records, "extension_event").len(), 1);
        plain.release().await;
    }

    #[tokio::test]
    async fn provider_account_events_map_to_upstream_records() {
        let session = session();
        let (handler, records) = handler(&session, &[]).await;
        maho_core::emit_provider_accounts_changed("rpc-account-fixture");
        maho_core::emit_provider_account_failover(
            "rpc-account-fixture",
            "first",
            "second",
            "rate_limit",
        );
        let mine = records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| record["provider"] == "rpc-account-fixture")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            mine,
            vec![
                serde_json::json!({"type":"auth_accounts_changed","provider":"rpc-account-fixture"}),
                serde_json::json!({"type":"account_failover","provider":"rpc-account-fixture","from":"first","to":"second","reason":"rate_limit"}),
            ]
        );
        handler.release().await;
        maho_core::emit_provider_accounts_changed("rpc-account-fixture");
        assert_eq!(
            records
                .lock()
                .expect("records")
                .iter()
                .filter(|record| record["provider"] == "rpc-account-fixture")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn commands_changed_suppresses_baseline_and_publishes_later_change() {
        let session = session();
        let (handler, records) = handler(&session, &[]).await;
        assert!(of_type(&records, "commands_changed").is_empty());
        session.emit_extension_event(
            maho_core::agent_session::SESSION_REBIND_CHANNEL,
            &serde_json::json!({"reason":"reload"}),
        );
        assert!(of_type(&records, "commands_changed").is_empty());
        let source_info = maho_core::source_info::create_synthetic_source_info(
            "rpc-commands-changed",
            maho_core::source_info::SyntheticSourceInfoOptions {
                source: "test".into(),
                ..Default::default()
            },
        );
        session.set_prompt_resources(
            vec![maho_core::prompt_templates::PromptTemplate {
                name: "review".into(),
                description: "Review work".into(),
                argument_hint: None,
                requires_arguments: false,
                content: "review".into(),
                source_info,
                file_path: "/tmp/review.md".into(),
            }],
            Vec::new(),
        );
        session.emit_extension_event(
            maho_core::agent_session::SESSION_REBIND_CHANNEL,
            &serde_json::json!({"reason":"reload"}),
        );
        let changed = of_type(&records, "commands_changed");
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["commands"][0]["name"], "review");
        assert!(of_type(&records, "session_replaced").is_empty());
        handler.release().await;
    }

    #[tokio::test]
    async fn session_replaced_announces_external_replacement_only() {
        let session = session();
        let (handler, records) = handler(&session, &[]).await;
        let response = handler
            .handle_input_line(&serde_json::json!({"id":"own","type":"new_session"}).to_string())
            .await
            .expect("dispatch")
            .expect("response");
        assert!(response.contains("\"success\":true"));
        assert!(of_type(&records, "session_replaced").is_empty());
        assert!(
            session
                .new_session(None)
                .await
                .expect("external replacement")
        );
        let replaced = of_type(&records, "session_replaced");
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0]["durableSessionId"], session.session_id());
        assert_eq!(
            replaced[0]["cwd"],
            if cfg!(windows) { "\\tmp" } else { "/tmp" }
        );
        handler.release().await;
    }

    #[tokio::test]
    async fn extension_shutdown_marks_the_connection_without_disposing() {
        let session = session();
        let (handler, _records) = handler(&session, &[]).await;
        assert!(!handler.is_shutdown_requested());
        maho_ext_api::ExtensionContextActions::shutdown(
            session.extension_context_actions().as_ref(),
        );
        assert!(handler.is_shutdown_requested());
        handler.release().await;
    }

    #[tokio::test]
    async fn component_widgets_render_only_for_rendered_components_clients() {
        let session = session();
        let (rendered, rendered_records) = handler(
            &session,
            &[crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY],
        )
        .await;
        let (plain, plain_records) = handler(&session, &[]).await;
        for ui in [rendered.ui().clone(), plain.ui().clone()] {
            maho_ext_api::ExtensionUi::set_widget(
                ui.as_ref(),
                "panel",
                Some(maho_ext_api::WidgetContent::Component(factory())),
                Default::default(),
            );
            maho_ext_api::ExtensionUi::set_header(ui.as_ref(), Some(factory()));
            maho_ext_api::ExtensionUi::set_footer(ui.as_ref(), None);
        }
        assert!(of_type(&plain_records, "extension_ui_request").is_empty());
        let requests = of_type(&rendered_records, "extension_ui_request");
        assert_eq!(
            requests
                .iter()
                .map(|record| record["method"].clone())
                .collect::<Vec<_>>(),
            vec![
                serde_json::json!("setWidget"),
                serde_json::json!("setHeader"),
                serde_json::json!("setFooter")
            ]
        );
        assert_eq!(requests[0]["widgetLines"], serde_json::json!(["row@80"]));
        assert_eq!(
            requests[0][crate::session_event_fanout::RENDERED_COMPONENT_RECORD],
            true
        );
        assert!(requests[2].get("widgetLines").is_none());
        let response = rendered
            .handle_input_line(
                &serde_json::json!({"id":"width","type":"set_client_info","width":40}).to_string(),
            )
            .await
            .expect("dispatch");
        assert!(response.is_some());
        let widths = of_type(&rendered_records, "extension_ui_request")
            .into_iter()
            .skip(3)
            .map(|record| record["widgetLines"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            widths,
            vec![serde_json::json!(["row@40"]), serde_json::json!(["row@40"])]
        );
        rendered.release().await;
        plain.release().await;
    }
}
#[cfg(test)]
mod tool_runtime_capability_tests {
    use super::*;
    use crate::host_capabilities::ToolRuntimeKind;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_classic_host_answers_with_its_own_runtime_and_drops_a_negotiated_one() {
        let capabilities = strings(&[
            "multi_session",
            "a",
            "b",
            "tool_runtime:child",
            "placement:offload",
            "offload_api",
            "custom:keep",
        ]);
        let advertised =
            classic_protocol_capabilities(&capabilities, Some(ToolRuntimeKind::InProcess), 3);
        assert_eq!(
            advertised,
            strings(&[
                "multi_session",
                "a",
                "b",
                "tool_runtime:in-process",
                "offload_api",
                "custom:keep"
            ])
        );
    }

    #[test]
    fn without_a_tool_runtime_a_classic_host_advertises_none_and_still_drops_negotiated_ones() {
        let capabilities = strings(&[
            "multi_session",
            "a",
            "b",
            "tool_runtime:child",
            "custom:keep",
        ]);
        let advertised = classic_protocol_capabilities(&capabilities, None, 3);
        assert_eq!(
            advertised,
            strings(&["multi_session", "a", "b", "custom:keep"])
        );
    }
}
