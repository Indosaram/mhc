#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Cursor;
use serde_json::{json, Value};
use ulw_loop_core::codex_hook::PreToolUsePayload;
use ulw_loop_core::spawn_guard::{apply_spawn_guards_with_env, run_spawn_guard_cli};
use std::collections::BTreeMap;
use ulw_loop_core::spawn_role_guard::LAZYCODEX_SPAWN_ROLES;

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    d
}
fn env_for(d: &tempfile::TempDir, key: &str) -> Option<String> {
    BTreeMap::from([
        (String::from("OMO_AGENT_TOOLKIT_SURFACE"), String::from("lazycodex")),
        (String::from("PLUGIN_DATA"), d.path().join("data").display().to_string()),
        (String::from("CODEX_HOME"), d.path().join("codex-home").display().to_string()),
    ])
    .get(key)
    .cloned()
}
fn guard(d: &tempfile::TempDir, tool: &str, input: Value) -> String {
    apply_spawn_guards_with_env(&PreToolUsePayload { cwd:d.path().display().to_string(), hook_event_name:"PreToolUse".into(), model:"test".into(), permission_mode:"default".into(), session_id:"matrix".into(), tool_input:input, tool_name:tool.into(), tool_use_id:"call".into(), transcript_path:None, turn_id:"turn".into() }, None, |key| env_for(d, key))
}
fn denied(s: &str) -> bool { serde_json::from_str::<Value>(s).unwrap()["hookSpecificOutput"]["permissionDecision"] == "deny" }

#[test]
fn native_spawns_cross_role_boundary_without_plan() {
    for tool in ["spawn_agent", "multi_agent_v1.spawn_agent", "collaborationspawn_agent", "collaboration.spawn_agent"] { for fork in [false, true] { for registered in [false, true] { for named in [false, true] {
        let d=setup(); if registered { fs::create_dir_all(d.path().join("codex-home/agents")).unwrap(); fs::write(d.path().join("codex-home/agents/worker.toml"), "name = \"lazycodex-worker-medium\"\n").unwrap(); }
        let mut input=json!({"message":"TASK: implement"}); if named { input["agent_type"]=json!(if registered {"lazycodex-worker-medium"} else {"worker"}); }
        if tool.contains("collaboration") { input["fork_turns"]=json!(if fork {"all"} else {"none"}); input["task_name"]=json!("work"); } else { input["fork_context"]=json!(fork); }
        let out=guard(&d,tool,input); assert_eq!(out.is_empty(), named && registered);
    }}}}
}
#[test] fn rejects_non_registry_or_malformed_agent_types() { let d=setup(); for v in [json!("default"),json!("deep"),json!("explore"),json!(""),json!("unknown"),json!(42),Value::Null] { assert!(denied(&guard(&d,"spawn_agent",json!({"agent_type":v})))); } }
#[test] fn rejects_omitted_fork_turns_and_role() { let d=setup(); assert!(denied(&guard(&d,"collaboration.spawn_agent",json!({"task_name":"work","message":"TASK: work"})))); }
#[test] fn does_not_change_senpi_role_admission() { let d=setup(); let input=json!({"agent_type":"deep"}); let payload=PreToolUsePayload { cwd:d.path().display().to_string(), hook_event_name:"PreToolUse".into(), model:"test".into(), permission_mode:"default".into(), session_id:"matrix".into(), tool_input:input, tool_name:"spawn_agent".into(), tool_use_id:"call".into(), transcript_path:None, turn_id:"turn".into() }; assert_eq!(apply_spawn_guards_with_env(&payload,None,|key| if key=="OMO_AGENT_TOOLKIT_SURFACE" { Some("omo-senpi".into()) } else { env_for(&d, key) }),""); }
#[test] fn explicit_selectors_match_bundled_names() { assert_eq!(LAZYCODEX_SPAWN_ROLES.len(),12); assert!(LAZYCODEX_SPAWN_ROLES.contains(&"explorer")); }
#[test] fn admits_hephaestus_examples() { let d=setup(); for role in ["explorer","lazycodex-worker-medium","librarian"] { assert_eq!(guard(&d,"multi_agent_v1.spawn_agent",json!({"agent_type":role,"message":"TASK: implement","fork_context":false})),""); } }
#[test] fn enforces_admission_breaker_after_role_validation() { let d=setup(); fs::create_dir_all(d.path().join("data/spawn-breaker")).unwrap(); fs::write(d.path().join("data/spawn-breaker/matrix.json"),json!({"reason":"capacity"}).to_string()).unwrap(); assert!(denied(&guard(&d,"spawn_agent",json!({"agent_type":"explorer"})))); }
#[test] fn malformed_input_denies_and_other_tools_pass() { let mut out=Vec::new(); run_spawn_guard_cli(Cursor::new("{bad-json"),&mut out); assert!(denied(std::str::from_utf8(&out).unwrap())); let d=setup(); assert_eq!(guard(&d,"read_file",json!({})),""); }

