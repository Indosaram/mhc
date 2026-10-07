//! The CLI host-facing member-scoped Kibitzer tools: `read`, `grep` and `session_entries`.
//!
//! Scan, cap, redaction and path-safety are the memory-owned shared implementation
//! (`maho_omo_memory::kibitzer_tools`); this module binds those executors to ONE bound session's
//! workspace root, session entries, caps and the session core's CURRENT-wake budget as registered
//! `ToolDefinition`s. The `memory`/`nudge` tools stay memory-owned.
//!
//! The binding is built PER SESSION by the memory composition (`KibitzerChildResourcesFactory`), so
//! two sessions never share a budget: `budget` resolves the session core's current
//! `KibitzerToolBudget` on each call.

use std::path::Path;
use std::sync::Arc;

use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use maho_omo_memory::kibitzer_contract::KibitzerToolBudget;
use maho_omo_memory::kibitzer_tools_grep::execute_grep;
use maho_omo_memory::kibitzer_tools_read::execute_read;
use maho_omo_memory::kibitzer_tools_session_read::session_entries_since;
use maho_omo_memory::kibitzer_tools_result::KibitzerToolResult;
use maho_omo_memory::kibitzer_tools_caps::{KibitzerToolCaps, DEFAULT_KIBITZER_TOOL_CAPS};

/// The bound parent session's RAW entries, oldest first; the tool applies the cursor and the caps.
pub type SessionEntriesResolver = Arc<dyn Fn() -> Vec<serde_json::Value> + Send + Sync>;

/// The CURRENT wake's budget for THIS bound session: the session core's current
/// `KibitzerToolBudget`, resolved through the binding's getter at call time. There is NO global slot.
pub type KibitzerBudgetSource = Arc<dyn Fn() -> Arc<dyn KibitzerToolBudget> + Send + Sync>;

/// One child's binding, built PER SESSION by the memory composition.
pub struct KibitzerHostToolsInput {
    pub cwd: String,
    pub session_entries: SessionEntriesResolver,
    pub caps: KibitzerToolCaps,
    pub budget: KibitzerBudgetSource,
}

/// The three host-facing member-scoped tools, in registry order (`read`, `grep`, `session_entries`).
pub fn create_kibitzer_host_tools(input: KibitzerHostToolsInput) -> Vec<ToolDefinition> {
    vec![read_tool(&input), grep_tool(&input), session_entries_tool(&input)]
}

/// Converts a shared tool result: an `is_error` result becomes the port's only model-visible error.
fn result_of(result: KibitzerToolResult) -> Result<ToolResult, ToolError> {
    if result.is_error {
        return Err(ToolError::Message(result.text));
    }
    let mut out = ToolResult::text(result.text.clone());
    // `terminate` rides in `details`: this port's `ToolResult` has no top-level terminate field, and
    // the child observation adapter reads `details.terminate` as the early-termination hint. The
    // rejected arm above stays the structured text code (the model-visible rejection body).
    out.details = Some(serde_json::json!({ "message": result.text, "terminate": result.terminate }));
    Ok(out)
}

/// Charges the session's current-wake budget; an exhausted budget is the shared structured rejection.
fn charge_or_reject(budget: &dyn KibitzerToolBudget) -> Result<(), ToolError> {
    if budget.charge() {
        return Ok(());
    }
    Err(ToolError::Message(
        serde_json::json!({
            "rejected": "tool_budget_exceeded",
            "message": format!("The tool-call budget for this wake ({}) is exhausted; end the turn.", budget.limit()),
        })
        .to_string(),
    ))
}

/// Wall-clock milliseconds for the grep scan budget (production default).
fn system_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0)
}

fn read_tool(input: &KibitzerHostToolsInput) -> ToolDefinition {
    let cwd = input.cwd.clone();
    let caps = input.caps;
    let budget = Arc::clone(&input.budget);
    let description = format!("Read a workspace file (bounded to {} characters; secrets are redacted).", caps.read_chars);
    let parameters = serde_json::json!({
        "type": "object", "additionalProperties": false, "required": ["path"],
        "properties": {
            "path": { "type": "string", "description": "File path relative to the workspace root." },
            "offset": { "type": "integer", "minimum": 1, "description": "1-based first line to return; defaults to 1." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to return." }
        }
    });
    ToolDefinition::new("read", &description, parameters, Arc::new(move |call| {
        let params = call.params.clone();
        let cwd = cwd.clone();
        let budget = Arc::clone(&budget);
        Box::pin(async move {
            charge_or_reject(budget().as_ref())?;
            result_of(execute_read(Path::new(&cwd), caps, &params))
        })
    }))
}

fn grep_tool(input: &KibitzerHostToolsInput) -> ToolDefinition {
    let cwd = input.cwd.clone();
    let caps = input.caps;
    let budget = Arc::clone(&input.budget);
    // The real gitignore-aware candidate list: the shared `system_git_exec` spawns `git ls-files`.
    let git = memory_core::git::exec::system_git_exec();
    let description = format!(
        "Search workspace files by regular expression (at most {} matching lines). Files ignored by .gitignore are skipped in a git workspace. The scan is bounded ({} files, {}MB read, {}s); a bounded run returns \"truncated\": true with \"stopped\" naming the limit it hit.",
        caps.grep_matches, caps.grep_scan_files, caps.grep_scan_bytes / (1024 * 1024), caps.grep_scan_ms / 1000,
    );
    let parameters = serde_json::json!({
        "type": "object", "additionalProperties": false, "required": ["pattern"],
        "properties": {
            "pattern": { "type": "string", "description": "JavaScript regular expression matched against each line." },
            "path": { "type": "string", "description": "File or directory relative to the workspace root; defaults to the root." },
            "ignore_case": { "type": "boolean", "description": "Case-insensitive match." }
        }
    });
    ToolDefinition::new("grep", &description, parameters, Arc::new(move |call| {
        let params = call.params.clone();
        // The turn's abort signal is the real `ToolCall.signal` (`AbortSignal`, `Clone`); the scan
        // checks it per directory and before every file. Cloned so the async block owns it.
        let signal = call.signal.clone();
        let cwd = cwd.clone();
        let budget = Arc::clone(&budget);
        let git = Arc::clone(&git);
        Box::pin(async move {
            charge_or_reject(budget().as_ref())?;
            let now = system_now_ms;
            // `execute_grep` reserves `Err` for the two upstream throws this port must not swallow
            // (an unresolvable workspace root, an unlistable directory): surface it as the call error
            // without inventing a rejection code. The budget is charged exactly once, above.
            match execute_grep(Path::new(&cwd), caps, &now, Some(&git), &params, Some(&signal)) {
                Ok(result) => result_of(result),
                Err(error) => Err(ToolError::Message(error.to_string())),
            }
        })
    }))
}

fn session_entries_tool(input: &KibitzerHostToolsInput) -> ToolDefinition {
    let resolve = Arc::clone(&input.session_entries);
    let caps = input.caps;
    let budget = Arc::clone(&input.budget);
    let description = format!("Read the primary agent's session entries after a cursor (at most {} per call).", caps.session_entries);
    let parameters = serde_json::json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "since": { "type": "integer", "minimum": -1, "description": "Cursor of the last entry already seen; -1 (default) starts from the beginning." }
        }
    });
    ToolDefinition::new("session_entries", &description, parameters, Arc::new(move |call| {
        let params = call.params.clone();
        let resolve = Arc::clone(&resolve);
        let budget = Arc::clone(&budget);
        Box::pin(async move {
            charge_or_reject(budget().as_ref())?;
            let since = params.get("since").and_then(serde_json::Value::as_i64).unwrap_or(-1);
            let page = session_entries_since(&resolve(), since, caps);
            result_of(maho_omo_memory::kibitzer_tools_result::ok_json(&page))
        })
    }))
}

/// `recall.tool_caps`: the resolved child-tool caps, field by field over the PINNED defaults (the
/// struct has no `Default`, so `DEFAULT_KIBITZER_TOOL_CAPS` is the base). Takes the RECALL BLOCK
/// (`resolve_agent_recall_settings` output), whose keys sit at the top level.
pub fn resolve_kibitzer_tool_caps(recall: &serde_json::Value) -> KibitzerToolCaps {
    let mut caps = DEFAULT_KIBITZER_TOOL_CAPS;
    let Some(block) = recall.get("tool_caps") else { return caps; };
    let field = |key: &str| block.get(key).and_then(serde_json::Value::as_u64);
    if let Some(value) = field("read_chars") { caps.read_chars = value as usize; }
    if let Some(value) = field("grep_matches") { caps.grep_matches = value as usize; }
    if let Some(value) = field("grep_line_chars") { caps.grep_line_chars = value as usize; }
    if let Some(value) = field("grep_scan_files") { caps.grep_scan_files = value as usize; }
    if let Some(value) = field("grep_scan_bytes") { caps.grep_scan_bytes = value as usize; }
    // `grep_scan_ms` is `i64` in the producer struct, so it is read with the signed accessor.
    if let Some(value) = block.get("grep_scan_ms").and_then(serde_json::Value::as_i64) { caps.grep_scan_ms = value; }
    if let Some(value) = field("session_entries") { caps.session_entries = value as usize; }
    if let Some(value) = field("session_entry_chars") { caps.session_entry_chars = value as usize; }
    if let Some(value) = field("memory_search_results") { caps.memory_search_results = value as usize; }
    if let Some(value) = field("memory_read_chars") { caps.memory_read_chars = value as usize; }
    caps
}

/// `recall.event_caps`: the resolved sidecar event caps, merged PER FIELD so a block that tightens
/// one cap keeps the others.
pub fn resolve_kibitzer_event_caps(recall: &serde_json::Value) -> maho_omo_memory::kibitzer_events::KibitzerEventCaps {
    let mut caps = maho_omo_memory::kibitzer_events::KibitzerEventCaps::default();
    let Some(block) = recall.get("event_caps") else { return caps; };
    let field = |key: &str| block.get(key).and_then(serde_json::Value::as_u64).map(|value| value as usize);
    if let Some(value) = field("tool_args") { caps.tool_args = value; }
    if let Some(value) = field("result_head") { caps.result_head = value; }
    if let Some(value) = field("assistant") { caps.assistant = value; }
    if let Some(value) = field("prompt") { caps.prompt = value; }
    caps
}
