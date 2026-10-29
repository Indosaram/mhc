//! The `record_fact` tool (latest `facts-record-tool.ts`).
//!
//! Provides the structured tool definition and execution for recording durable facts
//! directly into `extraction.jsonl` during facts extraction runs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use memory_core::facts::extraction::parse_facts_extraction_record;
use serde::{Deserialize, Serialize};

pub type FactsFailureCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// The registered tool name (`record_fact`).
pub const FACTS_RECORD_TOOL_NAME: &str = "record_fact";

/// Upstream tool label (`Record fact`).
pub const FACTS_RECORD_TOOL_LABEL: &str = "Record fact";

/// Upstream tool description.
pub const FACTS_RECORD_TOOL_DESCRIPTION: &str =
    "Record one durable fact extracted from the supplied conversation payload.";

/// Person reference parameter for `record_fact`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsRecordPersonParams {
    pub name: String,
    pub aliases: Vec<String>,
}

/// Parameters for `record_fact`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsRecordParams {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<FactsRecordPersonParams>,
    pub text: String,
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_quote: Option<String>,
}

/// Parameter schema for `record_fact`, matching upstream TypeBox definition.
pub fn facts_record_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["scope", "text", "date"],
        "properties": {
            "scope": {
                "type": "string",
                "enum": ["person", "project"]
            },
            "person": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "aliases"],
                "properties": {
                    "name": { "type": "string" },
                    "aliases": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            },
            "source": {"type":"string", "enum":["user","agent","inferred"]},
            "user_quote": {"type":"string"},
            "text": { "type": "string" },
            "date": { "type": "string" }
        }
    })
}

/// State tracking cancellation of a facts run.
#[derive(Clone, Debug, Default)]
pub struct FactsRecordRunState {
    pub cancelled: Arc<AtomicBool>,
}

/// Result of executing `record_fact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactsRecordToolResult {
    pub text: String,
    pub is_error: bool,
}

/// Input configuration for creating a `record_fact` tool instance.
pub struct FactsRecordToolInput {
    pub exchanges: Vec<crate::consent_exchange::ConsentExchange>,
    pub extraction_path: PathBuf,
    pub max_records: Option<usize>,
    pub max_bytes: Option<usize>,
    pub on_failure: Option<FactsFailureCallback>,
    pub state: Option<FactsRecordRunState>,
    /// Every call counts, rejected ones too: it bounds the model's tool turns, not only the records kept.
    pub max_calls: Option<usize>,
    pub on_call: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The stateful facts record tool instance.
pub struct FactsRecordTool {
    exchanges: Vec<crate::consent_exchange::ConsentExchange>,
    refused: std::sync::Mutex<std::collections::BTreeSet<String>>,
    ended: AtomicBool,
    active: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    max_calls: Option<usize>,
    on_call: Option<Arc<dyn Fn() + Send + Sync>>,
    bytes: Arc<AtomicUsize>,
    extraction_path: PathBuf,
    max_records: Option<usize>,
    max_bytes: Option<usize>,
    on_failure: Option<FactsFailureCallback>,
    state: Option<FactsRecordRunState>,
}

impl FactsRecordTool {
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &serde_json::Value,
    ) -> FactsRecordToolResult {
        if !self.active.load(Ordering::Acquire)
            || self
                .state
                .as_ref()
                .is_some_and(|s| s.cancelled.load(Ordering::Acquire))
        {
            return error_result("the facts run is no longer active");
        }

        let reject = |reason: &str| -> FactsRecordToolResult {
            if let Some(ref cb) = self.on_failure {
                cb(reason);
            }
            error_result(reason)
        };

        if let Some(max_calls) = self.max_calls
            && self.calls.load(Ordering::Acquire) >= max_calls
        {
            return reject(&format!(
                "the facts tool-call limit ({max_calls}) has been reached"
            ));
        }
        self.calls.fetch_add(1, Ordering::AcqRel);
        if let Some(on_call) = &self.on_call {
            on_call();
        }

        if self.ended.load(Ordering::Acquire) { return FactsRecordToolResult { text: "Fact rejected: the facts run has ended; stop calling record_fact".into(), is_error: true }; }
        // Source and quote are model input; the saved receipt is always computed locally.
        let mut fact = params.clone();
        let source = fact.get("source").and_then(serde_json::Value::as_str).unwrap_or("inferred");
        let quote = fact.get("user_quote").and_then(serde_json::Value::as_str);
        let receipt = self.exchanges.iter().find_map(|exchange| {
            let entries = [serde_json::json!({"role":"assistant", "text":exchange.proposal}), serde_json::json!({"role":"user", "text":exchange.reply})];
            let receipt = crate::memory_tool_provenance::resolve_claim_receipt(source, quote, &entries, None);
            (receipt.source == "user").then_some(receipt)
        }).unwrap_or_else(|| crate::memory_tool_provenance::resolve_claim_receipt(source, quote, &[], None));
        if let Some(object) = fact.as_object_mut() { object.remove("source"); object.remove("user_quote"); object.insert("receipt".into(), serde_json::json!(receipt)); }
        let record = match parse_facts_extraction_record(&fact, self.count.load(Ordering::Relaxed))
        {
            Ok(rec) => rec,
            Err(err) => {
                let signature = params.to_string();
                let first = self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(signature);
                if first { return reject(&format!("{}. Fix this call or skip this fact; the same call will not be accepted", err.message)); }
                self.ended.store(true, Ordering::Release);
                return reject(&format!("{}. This exact call was already refused, so the facts run ends here; facts recorded so far are kept", err.message));
            }
        };

        let line = match serde_json::to_string(&record) {
            Ok(json) => format!("{json}\n"),
            Err(err) => return reject(&err.to_string()),
        };
        let line_bytes = line.len();

        // Atomically check and reserve limits
        // Loop for count limit
        if let Some(max_records) = self.max_records {
            let mut curr = self.count.load(Ordering::Relaxed);
            loop {
                if curr >= max_records {
                    return reject(&format!(
                        "the facts run limit ({max_records}) has been reached"
                    ));
                }
                match self.count.compare_exchange_weak(
                    curr,
                    curr + 1,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => curr = actual,
                }
            }
        } else {
            self.count.fetch_add(1, Ordering::AcqRel);
        }

        // Loop for byte limit
        if let Some(max_bytes) = self.max_bytes {
            let mut curr = self.bytes.load(Ordering::Relaxed);
            loop {
                if curr + line_bytes > max_bytes {
                    return reject(&format!(
                        "the facts byte limit ({max_bytes}) has been reached"
                    ));
                }
                match self.bytes.compare_exchange_weak(
                    curr,
                    curr + line_bytes,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => curr = actual,
                }
            }
        } else {
            self.bytes.fetch_add(line_bytes, Ordering::AcqRel);
        }

        let record_number = self.count.load(Ordering::Relaxed);

        // Append line to file
        if let Err(err) = append_line(&self.extraction_path, &line) {
            return reject(&err.to_string());
        }

        FactsRecordToolResult {
            text: format!("Fact recorded ({record_number})."),
            is_error: false,
        }
    }

    pub fn to_tool_definition(self: Arc<Self>) -> ToolDefinition {
        let tool = Arc::clone(&self);
        ToolDefinition::new(
            FACTS_RECORD_TOOL_NAME,
            FACTS_RECORD_TOOL_DESCRIPTION,
            facts_record_parameters(),
            Arc::new(move |call| {
                let tool = Arc::clone(&tool);
                let params = call.params.clone();
                let call_id = call.id;
                Box::pin(async move {
                    let res = tool.execute(call_id, &params);
                    if tool.ended.load(Ordering::Acquire) {
                        let mut result = ToolResult::text(res.text);
                        result.is_error = Some(true);
                        result.details = Some(serde_json::json!({"terminate":true}));
                        Ok(result)
                    } else if res.is_error {
                        Err(ToolError::Message(res.text))
                    } else {
                        Ok(ToolResult::text(res.text))
                    }
                })
            }),
        )
    }
}

pub fn create_facts_record_tool(input: FactsRecordToolInput) -> FactsRecordTool {
    FactsRecordTool {
        exchanges: input.exchanges,
        refused: Default::default(),
        ended: AtomicBool::new(false),
        active: Arc::new(AtomicBool::new(true)),
        count: Arc::new(AtomicUsize::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
        max_calls: input.max_calls,
        on_call: input.on_call,
        bytes: Arc::new(AtomicUsize::new(0)),
        extraction_path: input.extraction_path,
        max_records: input.max_records,
        max_bytes: input.max_bytes,
        on_failure: input.on_failure,
        state: input.state,
    }
}

pub fn payload_exchanges(payload: &memory_core::facts::FactsPayload) -> Vec<crate::consent_exchange::ConsentExchange> {
    let mut exchanges = Vec::new();
    for entry in &payload.entries {
        let mut proposal: Option<String> = entry.lead.as_ref().and_then(|item| match item {
            memory_core::journal::entries::TranscriptEntry::Text(text) if text.kind == "assistant" && !text.text.trim().is_empty() => Some(text.text.clone()),
            _ => None,
        });
        for item in &entry.entries {
            if item.kind() == "reasoning" { continue; }
            let text = match item { memory_core::journal::entries::TranscriptEntry::Text(text) => text.text.as_str(), _ => "" };
            if item.kind() == "user" && !text.trim().is_empty()
                && let Some(proposal) = &proposal { exchanges.push(crate::consent_exchange::ConsentExchange { proposal: proposal.clone(), reply: text.into() }); }
            proposal = (item.kind() == "assistant" && !text.trim().is_empty()).then(|| text.to_owned());
        }
    }
    exchanges
}

fn error_result(reason: &str) -> FactsRecordToolResult {
    FactsRecordToolResult {
        text: format!("Fact rejected: {reason}"),
        is_error: true,
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

#[cfg(test)]
#[path = "facts_record_tool_tests.rs"]
mod tests;
