//! Port of `claim-receipt/evidence.ts`: build the position-ordered check and edit timeline of a
//! session branch.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use maho_omo_post_mutation::post_mutation::{extract_mutated_file_paths, MUTATION_TOOL_NAMES};
use maho_omo_post_mutation::PostMutationEvent;

use crate::cell_commands::{cell_invoked_kinds, CellCheckKind};
use crate::invocations::{invoked_kinds, InvokedKind};
use crate::signatures::{is_relevant_path, judge_output, judge_structured};

/// The kinds of evidence a session can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckKind {
    Test,
    Typecheck,
    Lint,
    Build,
    Run,
    Lsp,
    Lookup,
}

impl CheckKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Typecheck => "typecheck",
            Self::Lint => "lint",
            Self::Build => "build",
            Self::Run => "run",
            Self::Lsp => "lsp",
            Self::Lookup => "lookup",
        }
    }

    #[must_use]
    pub const fn from_invoked(kind: InvokedKind) -> Option<Self> {
        match kind {
            InvokedKind::Test => Some(Self::Test),
            InvokedKind::Typecheck => Some(Self::Typecheck),
            InvokedKind::Lint => Some(Self::Lint),
            InvokedKind::Build => Some(Self::Build),
            InvokedKind::Run => Some(Self::Run),
            InvokedKind::Lookup => None,
        }
    }
}

/// What a check's output says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckOutcome {
    Passed,
    Failed,
    Unknown,
}

impl CheckOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// One check run, at its branch position.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckEvidence {
    pub at: f64,
    pub entry_id: String,
    pub kind: CheckKind,
    pub outcome: CheckOutcome,
    pub command: Option<String>,
}

/// One edit, at its branch position.
#[derive(Clone, Debug, PartialEq)]
pub struct EditEvidence {
    pub at: f64,
    pub entry_id: String,
    pub paths: Vec<String>,
    pub relevant: bool,
}

/// The session's check and edit evidence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub checks: Vec<CheckEvidence>,
    pub edits: Vec<EditEvidence>,
}

const LOOKUP_TOOLS: [&str; 9] = [
    "read",
    "grep",
    "glob",
    "find",
    "ls",
    "lsp_goto_definition",
    "lsp_find_references",
    "lsp_symbols",
    "ast_grep",
];

static BACKGROUND_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bID:\s*([\w-]+)").expect("valid background id regex"));
static DETACHED_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Detached eval cell (\S+) \(\w+\) (completed|failed)")
        .expect("valid detached done regex")
});
static TERMINAL_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Background terminal session (\S+) finished: (\w+) \(exit code (-?\d+)\)")
        .expect("valid terminal done regex")
});
static MONITOR_DESCRIPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Monitor event\(([^)]*)\)").expect("valid monitor description regex"));
static WATCHER_ENDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bwatcher (?:killed|timed_out)\b|\bwatcher\b[^\n]*\bkilled\b")
        .expect("valid watcher regex")
});

/// Upstream `contentText`: a string content, or the text parts of a content array joined by newlines.
#[must_use]
pub fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    match content.as_array() {
        Some(parts) => parts
            .iter()
            .map(|part| {
                if part["type"].as_str() == Some("text") {
                    part["text"].as_str().unwrap_or("").to_owned()
                } else {
                    String::new()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingCheck {
    kinds: Vec<CheckKind>,
    command: String,
}

fn split_kinds(line: &str) -> (Vec<CheckKind>, bool) {
    let all = invoked_kinds(line, 0);
    let lookup = all.contains(&InvokedKind::Lookup);
    let kinds = all
        .into_iter()
        .filter_map(CheckKind::from_invoked)
        .collect();
    (kinds, lookup)
}

fn as_string(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}

fn check_kind_of(kind: CellCheckKind) -> CheckKind {
    match kind {
        CellCheckKind::Test => CheckKind::Test,
        CellCheckKind::Typecheck => CheckKind::Typecheck,
        CellCheckKind::Lint => CheckKind::Lint,
        CellCheckKind::Build => CheckKind::Build,
        CellCheckKind::Run => CheckKind::Run,
    }
}

struct TimelineBuilder {
    checks: Vec<CheckEvidence>,
    edits: Vec<EditEvidence>,
    calls: HashMap<String, (String, Value)>,
    background: HashMap<String, PendingCheck>,
    detached: HashMap<String, PendingCheck>,
}

impl TimelineBuilder {
    fn new() -> Self {
        Self {
            checks: Vec::new(),
            edits: Vec::new(),
            calls: HashMap::new(),
            background: HashMap::new(),
            detached: HashMap::new(),
        }
    }

    fn add_check(
        &mut self,
        at: f64,
        entry_id: &str,
        pending: &PendingCheck,
        outcome: CheckOutcome,
    ) {
        let command = pending.command.chars().take(200).collect::<String>();
        for kind in &pending.kinds {
            self.checks.push(CheckEvidence {
                at,
                entry_id: entry_id.to_owned(),
                kind: *kind,
                outcome,
                command: Some(command.clone()),
            });
        }
    }

    fn add_edit(&mut self, at: f64, entry_id: &str, paths: Vec<String>) {
        let relevant = paths.is_empty() || paths.iter().any(|path| is_relevant_path(path));
        self.edits.push(EditEvidence {
            at,
            entry_id: entry_id.to_owned(),
            paths,
            relevant,
        });
    }

    fn entry(&mut self, entry: &Value, index: usize) {
        if !entry.is_object() {
            return;
        }
        let entry_id = as_string(&entry["id"]);
        if entry["type"].as_str() == Some("custom_message") {
            let custom_type = as_string(&entry["customType"]);
            let content = content_text(&entry["content"]);
            let details = &entry["details"];
            self.notification(&custom_type, &content, details, index, &entry_id);
            return;
        }
        let message = &entry["message"];
        if entry["type"].as_str() != Some("message") || !message.is_object() {
            return;
        }
        if message["role"].as_str() == Some("assistant") {
            if let Some(parts) = message["content"].as_array() {
                for part in parts {
                    if part["type"].as_str() == Some("toolCall") {
                        let arguments = if part["arguments"].is_object() {
                            part["arguments"].clone()
                        } else {
                            Value::Object(serde_json::Map::new())
                        };
                        self.calls
                            .insert(as_string(&part["id"]), (as_string(&part["name"]), arguments));
                    }
                }
            }
            return;
        }
        if message["role"].as_str() == Some("toolResult") {
            self.tool_result(message, index, &entry_id);
        }
    }

    fn tool_result(&mut self, message: &Value, index: usize, entry_id: &str) {
        let tool_name = as_string(&message["toolName"]);
        let call_id = as_string(&message["toolCallId"]);
        let empty = Value::Object(serde_json::Map::new());
        let args = self.calls.get(&call_id).map_or_else(
            || empty.clone(),
            |(_, arguments)| arguments.clone(),
        );
        let is_error = message["isError"] == Value::Bool(true);
        let text = content_text(&message["content"]);
        if tool_name == "eval" {
            self.eval_result(call_id.as_str(), &args, &message["details"], &text, is_error, (index, entry_id));
            return;
        }
        if MUTATION_TOOL_NAMES.contains(&tool_name.as_str()) {
            if !is_error {
                let event =
                    PostMutationEvent::new(tool_name, args.clone(), Some(message["details"].clone()));
                self.add_edit(index as f64, entry_id, extract_mutated_file_paths(&event));
            }
            return;
        }
        if tool_name == "lsp_diagnostics" {
            self.lsp(&text, is_error, index as f64, entry_id);
            return;
        }
        if LOOKUP_TOOLS.contains(&tool_name.as_str()) {
            self.lookup(is_error, &text, index as f64, entry_id);
            return;
        }
        if tool_name == "bash" || tool_name == "powershell" {
            let command = as_string(&args["command"]);
            let (kinds, lookup) = split_kinds(&command);
            if !kinds.is_empty() {
                self.add_check(
                    index as f64,
                    entry_id,
                    &PendingCheck { kinds, command },
                    judge_structured(is_error, &text),
                );
            } else if lookup {
                self.lookup(is_error, &text, index as f64, entry_id);
            }
        }
    }

    fn lookup(&mut self, is_error: bool, text: &str, at: f64, entry_id: &str) {
        if !is_error && !text.trim().is_empty() {
            self.checks.push(CheckEvidence {
                at,
                entry_id: entry_id.to_owned(),
                kind: CheckKind::Lookup,
                outcome: CheckOutcome::Passed,
                command: None,
            });
        }
    }

    fn lsp(&mut self, text: &str, is_error: bool, at: f64, entry_id: &str) {
        if is_error {
            return;
        }
        let outcome = judge_output(text);
        if outcome != CheckOutcome::Unknown {
            self.checks.push(CheckEvidence {
                at,
                entry_id: entry_id.to_owned(),
                kind: CheckKind::Lsp,
                outcome,
                command: None,
            });
        }
    }

    fn eval_result(
        &mut self,
        call_id: &str,
        args: &Value,
        details: &Value,
        text: &str,
        is_error: bool,
        position: (usize, &str),
    ) {
        let (index, entry_id) = position;
        let null = Value::Null;
        let record = if details.is_object() {
            details
        } else {
            &null
        };
        let host_calls: Vec<&Value> = record["toolCalls"]
            .as_array()
            .map(|calls| calls.iter().filter(|call| call.is_object()).collect())
            .unwrap_or_default();
        for call in host_calls.iter().copied() {
            self.host_call(call, index as f64 + 0.5, entry_id);
        }
        if host_calls
            .iter()
            .any(|call| call["name"].as_str() == Some("bash"))
        {
            return;
        }
        let cells: Vec<&Value> = record["cells"]
            .as_array()
            .map(|cells| cells.iter().filter(|cell| cell.is_object()).collect())
            .unwrap_or_default();
        if cells.is_empty() {
            let status = if is_error { "error" } else { "complete" };
            self.cell(
                call_id,
                args["code"].as_str().unwrap_or(""),
                text,
                Some(status),
                index,
                entry_id,
            );
            return;
        }
        for cell in cells {
            self.cell(
                call_id,
                cell["code"].as_str().unwrap_or(""),
                cell["output"].as_str().unwrap_or(""),
                cell["status"].as_str(),
                index,
                entry_id,
            );
        }
    }

    fn host_call(&mut self, call: &Value, at: f64, entry_id: &str) {
        let name = as_string(&call["name"]);
        let empty = Value::Object(serde_json::Map::new());
        let args = if call["args"].is_object() {
            &call["args"]
        } else {
            &empty
        };
        let preview = as_string(&call["resultPreview"]);
        let failed = call["ok"] == Value::Bool(false);
        if MUTATION_TOOL_NAMES.contains(&name.as_str()) {
            if !failed {
                let event = PostMutationEvent::new(name, args.clone(), None);
                self.add_edit(at, entry_id, extract_mutated_file_paths(&event));
            }
            return;
        }
        if name == "lsp_diagnostics" {
            self.lsp(&preview, failed, at, entry_id);
            return;
        }
        if LOOKUP_TOOLS.contains(&name.as_str()) {
            self.lookup(failed, &preview, at, entry_id);
            return;
        }
        if name != "bash" && name != "monitor" {
            return;
        }
        let command = as_string(&args["command"]);
        let (kinds, lookup) = split_kinds(&command);
        if kinds.is_empty() {
            if lookup && name == "bash" {
                self.lookup(failed, &preview, at, entry_id);
            }
            return;
        }
        let background = name == "monitor" || args["run_in_background"] == Value::Bool(true);
        if background && failed {
            return;
        }
        let background_id = if background {
            BACKGROUND_ID
                .captures(&preview)
                .and_then(|captures| captures.get(1))
                .map(|matched| matched.as_str().to_owned())
        } else {
            None
        };
        let pending = PendingCheck { kinds, command };
        if let Some(background_id) = background_id {
            self.background.insert(background_id, pending.clone());
            if name == "monitor" {
                let description = as_string(&args["description"]);
                self.background
                    .insert(format!("desc:{description}"), pending);
            }
            return;
        }
        self.add_check(at, entry_id, &pending, judge_structured(failed, &preview));
    }

    fn cell(
        &mut self,
        call_id: &str,
        code: &str,
        output: &str,
        status: Option<&str>,
        index: usize,
        entry_id: &str,
    ) {
        let invoked = cell_invoked_kinds(code);
        if invoked.kinds.is_empty() {
            return;
        }
        let kinds: Vec<CheckKind> = invoked
            .kinds
            .iter()
            .map(|kind| check_kind_of(*kind))
            .collect();
        let pending = PendingCheck {
            kinds,
            command: code.to_owned(),
        };
        let at = index as f64 + (invoked.offset as f64 + 1.0) / (code.len() as f64 + 2.0);
        // Cell code mixes mutation probes, log greps, scratch writes and composed reports with real
        // runs, so a cell can support a claim but never contradicts one: its failures are not
        // attributed and its writes are not edits. Only structured sources (host calls, tool
        // results, terminal exit codes) produce a failed check, and only edit/write/apply_patch
        // results produce an edit.
        if status == Some("complete") && judge_output(output) == CheckOutcome::Passed {
            self.add_check(at, entry_id, &pending, CheckOutcome::Passed);
            return;
        }
        if status == Some("complete") || status == Some("error") {
            self.add_check(at, entry_id, &pending, CheckOutcome::Unknown);
            return;
        }
        self.detached.insert(call_id.to_owned(), pending.clone());
        self.add_check(at, entry_id, &pending, CheckOutcome::Unknown);
    }

    fn notification(
        &mut self,
        custom_type: &str,
        content: &str,
        details: &Value,
        index: usize,
        entry_id: &str,
    ) {
        if custom_type == "omo-senpi:wake" {
            if let Some(items) = details.as_array() {
                for item in items.iter().filter(|item| item.is_object()) {
                    let range = item["contentRange"].as_array();
                    let start = range
                        .and_then(|range| range.first())
                        .and_then(Value::as_u64);
                    let end = range.and_then(|range| range.get(1)).and_then(Value::as_u64);
                    let slice = match (start, end) {
                        (Some(start), Some(end)) => content
                            .get(start as usize..end as usize)
                            .unwrap_or(content)
                            .to_owned(),
                        _ => content.to_owned(),
                    };
                    let nested_type = as_string(&item["customType"]);
                    let nested_details = &item["details"];
                    self.notification(&nested_type, &slice, nested_details, index, entry_id);
                }
            }
            return;
        }
        if custom_type == "senpi-codemode:notification" {
            let Some(captures) = DETACHED_DONE.captures(content) else {
                return;
            };
            let call_id = captures.get(1).map_or("", |matched| matched.as_str());
            let Some(pending) = self.detached.remove(call_id) else {
                return;
            };
            let completed = captures.get(2).map_or("", |matched| matched.as_str()) == "completed";
            let tail = content
                .get(captures.get(0).map_or(0, |matched| matched.end())..)
                .unwrap_or("");
            let passed = completed && judge_output(tail) == CheckOutcome::Passed;
            self.add_check(
                index as f64,
                entry_id,
                &pending,
                if passed {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Unknown
                },
            );
            return;
        }
        if custom_type == "senpi-terminal:notification" {
            let Some(captures) = TERMINAL_DONE.captures(content) else {
                return;
            };
            let session_id = captures.get(1).map_or("", |matched| matched.as_str());
            let Some(pending) = self.background.remove(session_id) else {
                return;
            };
            let state = captures.get(2).map_or("", |matched| matched.as_str());
            if state == "killed" {
                return;
            }
            let exit_code = captures.get(3).map_or("", |matched| matched.as_str());
            let outcome = if exit_code == "0" {
                judge_output(content)
            } else {
                CheckOutcome::Failed
            };
            self.add_check(index as f64, entry_id, &pending, outcome);
            return;
        }
        if custom_type == "senpi-monitor:notification" {
            if WATCHER_ENDED.is_match(content) {
                return;
            }
            let ids: Vec<String> = details["monitors"]
                .as_array()
                .map(|monitors| {
                    monitors
                        .iter()
                        .map(|monitor| as_string(&monitor["id"]))
                        .collect()
                })
                .unwrap_or_default();
            let description = MONITOR_DESCRIPTION
                .captures(content)
                .and_then(|captures| captures.get(1))
                .map(|matched| matched.as_str().to_owned());
            let pending = ids
                .iter()
                .find_map(|id| self.background.get(id).cloned())
                .or_else(|| {
                    description.and_then(|description| {
                        self.background.get(&format!("desc:{description}")).cloned()
                    })
                });
            let outcome = judge_output(content);
            if let Some(pending) = pending
                && outcome != CheckOutcome::Unknown
            {
                self.add_check(index as f64, entry_id, &pending, outcome);
            }
        }
    }
}

#[must_use]
pub fn build_timeline(entries: &[Value]) -> Timeline {
    let mut builder = TimelineBuilder::new();
    for (index, entry) in entries.iter().enumerate() {
        builder.entry(entry, index);
    }
    Timeline {
        checks: builder.checks,
        edits: builder.edits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_entries(call_id: &str, code: &str, output: &str, status: &str) -> Vec<Value> {
        serde_json::json!([
            {
                "type": "message",
                "id": "call",
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "toolCall", "id": call_id, "name": "eval", "arguments": { "code": code } }]
                }
            },
            {
                "type": "message",
                "id": "result",
                "message": {
                    "role": "toolResult",
                    "toolCallId": call_id,
                    "toolName": "eval",
                    "content": [{ "type": "text", "text": output }],
                    "details": {
                        "cells": [{ "index": 0, "code": code, "output": output, "status": status }],
                        "toolCalls": []
                    },
                    "isError": status == "error"
                }
            }
        ])
        .as_array()
        .cloned()
        .unwrap_or_default()
    }

    #[test]
    fn a_cell_can_support_a_claim_but_never_contradict_one() {
        let passed = build_timeline(&eval_entries("c1", "await sh(`bun test x`)", " 35 pass\n 0 fail", "complete"));
        assert_eq!(passed.checks.len(), 1);
        assert_eq!(passed.checks[0].kind, CheckKind::Test);
        assert_eq!(passed.checks[0].outcome, CheckOutcome::Passed);

        let red = build_timeline(&eval_entries("c2", "await sh(`bun test x`)", " 3 pass\n 1 fail", "complete"));
        assert_eq!(red.checks[0].outcome, CheckOutcome::Unknown);
        assert_eq!(red.edits.len(), 0);
    }

    #[test]
    fn a_detached_cell_resolves_from_its_completion_notification() {
        let mut entries = eval_entries("call_7", "await sh(`bun run typecheck`)", "", "detached");
        entries.push(serde_json::json!({
            "type": "custom_message",
            "id": "notice",
            "customType": "senpi-codemode:notification",
            "content": "<system-reminder>Detached eval cell call_7 (js) completed.\nTYPECHECK_EXIT=0</system-reminder>"
        }));
        let timeline = build_timeline(&entries);
        assert_eq!(timeline.checks.len(), 2);
        assert_eq!(timeline.checks[1].outcome, CheckOutcome::Passed);
        assert_eq!(timeline.checks[1].kind, CheckKind::Typecheck);
    }

    #[test]
    fn notes_edits_are_not_relevant() {
        let entries = serde_json::json!([
            {
                "type": "message",
                "id": "a",
                "message": { "role": "assistant", "content": [{ "type": "toolCall", "id": "e2", "name": "edit", "arguments": { "path": ".omo/evidence/run/summary.md" } }] }
            },
            {
                "type": "message",
                "id": "b",
                "message": { "role": "toolResult", "toolCallId": "e2", "toolName": "edit", "content": [{ "type": "text", "text": "ok" }], "details": {} }
            }
        ]);
        let timeline = build_timeline(entries.as_array().cloned().unwrap_or_default().as_slice());
        assert_eq!(timeline.edits.len(), 1);
        assert!(!timeline.edits[0].relevant);
    }
}
