//! The `record_fact` tool (latest `facts-record-tool.ts`).
//!
//! Provides the structured tool definition and execution for recording durable facts
//! directly into `extraction.jsonl` during facts extraction runs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

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
    pub extraction_path: PathBuf,
    pub max_records: Option<usize>,
    pub max_bytes: Option<usize>,
    pub on_failure: Option<FactsFailureCallback>,
    pub state: Option<FactsRecordRunState>,
}

/// The stateful facts record tool instance.
pub struct FactsRecordTool {
    active: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
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

    pub fn execute(&self, _tool_call_id: &str, params: &serde_json::Value) -> FactsRecordToolResult {
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

        // Parse and validate using core extraction record validation
        let record = match parse_facts_extraction_record(params, self.count.load(Ordering::Relaxed)) {
            Ok(rec) => rec,
            Err(err) => return reject(&err.message),
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
                    if res.is_error {
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
        active: Arc::new(AtomicBool::new(true)),
        count: Arc::new(AtomicUsize::new(0)),
        bytes: Arc::new(AtomicUsize::new(0)),
        extraction_path: input.extraction_path,
        max_records: input.max_records,
        max_bytes: input.max_bytes,
        on_failure: input.on_failure,
        state: input.state,
    }
}

fn error_result(reason: &str) -> FactsRecordToolResult {
    FactsRecordToolResult {
        text: format!("Fact rejected: {reason}"),
        is_error: true,
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

#[cfg(test)]
#[path = "facts_record_tool_tests.rs"]
mod tests;
