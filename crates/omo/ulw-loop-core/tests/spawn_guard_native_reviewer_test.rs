#![allow(clippy::unwrap_used)]
use std::fs;
use serde_json::{json, Value};
use ulw_loop_core::codex_hook::PreToolUsePayload;
use ulw_loop_core::spawn_guard::apply_spawn_budget_guards_with_env;
use std::path::Path;

fn payload(cwd: &Path, tool: &str, input: Value) -> PreToolUsePayload { PreToolUsePayload { hook_event_name: "PreToolUse".into(), session_id: "s1".into(), turn_id: "t1".into(), transcript_path: None, cwd: cwd.display().to_string(), model: "gpt-5.6-sol".into(), permission_mode: "default".into(), tool_name: tool.into(), tool_use_id: "tu1".into(), tool_input: input } }
fn write_goals(dir: &Path) { let session = dir.join(".omo/ulw-loop/s1"); fs::create_dir_all(&session).unwrap(); fs::write(session.join("goals.json"), json!({"version":1,"createdAt":"2026-07-11T00:00:00.000Z","updatedAt":"2026-07-11T00:00:00.000Z","briefPath":".omo/ulw-loop/s1/brief.md","goalsPath":".omo/ulw-loop/s1/goals.json","ledgerPath":".omo/ulw-loop/s1/ledger.jsonl","codexGoalMode":"aggregate","goals":[{"id":"g1","title":"Final goal","objective":"Final goal","status":"in_progress","successCriteria":[criterion("C001"),criterion("C002")],"attempt":1,"createdAt":"2026-07-11T00:00:00.000Z","updatedAt":"2026-07-11T00:00:00.000Z"}],"activeGoalId":"g1"}).to_string()).unwrap(); }
fn criterion(id: &str) -> Value { json!({"id":id,"scenario":format!("scenario {id}"),"userModel":"happy","expectedEvidence":"evidence","capturedEvidence":"captured","status":"pass","capturedAt":"2026-07-11T00:00:00.000Z"}) }
fn deny_reason(output: &str) -> String { serde_json::from_str::<Value>(output).unwrap()["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().into() }
fn gate(dir: &Path, tool: &str, input: Value) -> String { ulw_loop_core::spawn_guard::apply_spawn_budget_guards(&payload(dir, tool, input), None) }
fn senpi_gate(dir: &Path, tool: &str, input: Value) -> String { apply_spawn_budget_guards_with_env(&payload(dir, tool, input), None, |key| (key == "OMO_AGENT_TOOLKIT_SURFACE").then(|| "omo-senpi".into())) }

#[test]
fn canonical_gate_reviewer_without_manual_qa_is_denied() { let dir = tempfile::tempdir().unwrap(); write_goals(dir.path()); let out = gate(dir.path(), "collaborationspawn_agent", json!({"agent_type":"omo-native-gate-reviewer","message":"audit"})); assert!(!out.is_empty()); assert!(deny_reason(&out).contains("g1-manual-qa.md")); }
#[test]
fn canonical_gate_reviewer_with_manual_qa_is_allowed() { let dir = tempfile::tempdir().unwrap(); write_goals(dir.path()); fs::create_dir_all(dir.path().join(".omo/evidence")).unwrap(); fs::write(dir.path().join(".omo/evidence/g1-manual-qa.md"), "matrix\n").unwrap(); assert!(gate(dir.path(), "collaborationspawn_agent", json!({"agent_type":"omo-native-gate-reviewer","message":"audit"})).is_empty()); }
#[test]
fn message_only_canonical_gate_reviewer_is_recognized() { let dir = tempfile::tempdir().unwrap(); write_goals(dir.path()); let out = gate(dir.path(), "spawn_agent", json!({"message":"Act as omo-native-gate-reviewer; audit"})); assert!(!out.is_empty()); assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny"); }
#[test]
fn fourth_canonical_gate_spawn_is_denied_by_cap() { let dir = tempfile::tempdir().unwrap(); write_goals(dir.path()); fs::create_dir_all(dir.path().join(".omo/evidence")).unwrap(); fs::write(dir.path().join(".omo/evidence/g1-code-review.md"), "report\n").unwrap(); fs::write(dir.path().join(".omo/evidence/g1-manual-qa.md"), "matrix\n").unwrap(); let input = json!({"agent_type":"omo-native-gate-reviewer","message":"audit"}); for _ in 0..3 { assert!(senpi_gate(dir.path(), "spawn_agent", input.clone()).is_empty()); } let out = senpi_gate(dir.path(), "spawn_agent", input); assert!(deny_reason(&out).contains("4/3")); assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(dir.path().join(".omo/ulw-loop/s1/review-spawn-counts.json")).unwrap()).unwrap()["omo-native-gate-reviewer:g1:a1"], 3); }
#[test]
fn legacy_gate_reviewer_without_manual_qa_remains_denied() { let dir = tempfile::tempdir().unwrap(); write_goals(dir.path()); let out = gate(dir.path(), "collaborationspawn_agent", json!({"agent_type":"omo-senpi-gate-reviewer","message":"audit"})); assert!(deny_reason(&out).contains("g1-manual-qa.md")); }

