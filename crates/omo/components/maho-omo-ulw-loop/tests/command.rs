use maho_omo_ulw_loop::omo_command::*;
#[test] fn windows_cmd_wraps() { let t=to_spawn_target("C:\\bin\\omo.cmd",&["status".into()],"win32","bun");assert_eq!(t.command,"cmd.exe");assert_eq!(t.args,["/d","/s","/c","C:\\bin\\omo.cmd","status"]); }
#[test] fn uppercase_bat_wraps() { assert_eq!(to_spawn_target("D:\\OMO.BAT",&[],"win32","bun").command,"cmd.exe"); }
#[test] fn exe_direct() { assert_eq!(to_spawn_target("C:\\omo.exe",&[],"win32","bun").command,"C:\\omo.exe"); }
#[test] fn darwin_plain_direct() { assert_eq!(to_spawn_target("/bin/omo-agent-toolkit",&[],"darwin","bun").command,"/bin/omo-agent-toolkit"); }
#[test] fn linux_cmd_direct() { assert_eq!(to_spawn_target("/bin/omo.cmd",&[],"linux","bun").command,"/bin/omo.cmd"); }
#[test] fn windows_js_runtime() { let t=to_spawn_target("C:\\omo.js",&["status".into()],"win32","bun");assert_eq!(t.command,"bun");assert_eq!(t.args,["C:\\omo.js","status"]); }
#[test] fn darwin_js_runtime() { assert_eq!(to_spawn_target("/omo.js",&[],"darwin","bun").command,"bun"); }
#[test] fn linux_js_runtime() { assert_eq!(to_spawn_target("/omo.js",&[],"linux","bun").command,"bun"); }
#[test] fn uppercase_js_runtime() { assert_eq!(to_spawn_target("D:\\OMO.JS",&[],"win32","bun").command,"bun"); }
#[test] fn toolkit_override_precedes_path() { let e=std::collections::BTreeMap::from([("OMO_AGENT_TOOLKIT_BIN".into()," /custom/toolkit ".into()),("OMO_BIN".into(),"/legacy".into())]);assert_eq!(resolve_omo_bin(&e,|_|Some("/path/toolkit".into())).as_deref(),Some("/custom/toolkit")); }
#[test] fn toolkit_path_precedes_legacy_override() { let e=std::collections::BTreeMap::from([("OMO_BIN".into(),"/legacy".into())]);assert_eq!(resolve_omo_bin(&e,|name|{assert_eq!(name,"omo-agent-toolkit");Some("/path/toolkit".into())}).as_deref(),Some("/path/toolkit")); }
#[test] fn no_bare_omo_lookup() { assert!(resolve_omo_bin(&Default::default(),|name|{assert_eq!(name,"omo-agent-toolkit");None}).is_none()); }
#[test] fn active_pending_goal() { assert!(maho_omo_ulw_loop::index::status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"goals":[{"status":"pending"}]}}))); }
#[test] fn blocked_goal_not_active() { assert!(!maho_omo_ulw_loop::index::status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"goals":[{"status":"pending","steeringStatus":"blocked"}]}}))); }
#[test] fn aggregate_complete_not_active() { assert!(!maho_omo_ulw_loop::index::status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"aggregateCompletion":{"status":"complete"},"goals":[{"status":"pending"}]}}))); }
#[test]
fn whitespace_override_falls_through_to_legacy() {
    let env = std::collections::BTreeMap::from([("OMO_AGENT_TOOLKIT_BIN".into(), "   ".into()),
        ("OMO_BIN".into(), " /legacy ".into())]);
    assert_eq!(resolve_omo_bin(&env, |_| None).as_deref(), Some("/legacy"));
}
#[test]
fn completed_criteria_and_superseded_goals_are_inactive() {
    for goal in [serde_json::json!({"status":"pending","successCriteria":[{"status":"pass"}]}),
        serde_json::json!({"status":"in_progress","steeringStatus":"superseded"}),
        serde_json::json!({"status":"complete"}), serde_json::Value::Null] {
        assert!(!maho_omo_ulw_loop::index::status_has_active_incomplete_run(
            &serde_json::json!({"ok":true,"plan":{"goals":[goal]}})));
    }
}
#[test]
fn empty_and_malformed_status_are_inactive() {
    for value in [serde_json::Value::Null, serde_json::json!({"ok":false}),
        serde_json::json!({"ok":true,"plan":{"goals":[]}}),
        serde_json::json!({"ok":true,"plan":{"goals":{}}})] {
        assert!(!maho_omo_ulw_loop::index::status_has_active_incomplete_run(&value));
    }
}
#[test]
fn incomplete_criteria_keep_in_progress_goal_active() {
    for criteria in [serde_json::json!([]), serde_json::json!([{"status":"fail"}]),
        serde_json::json!([null]), serde_json::json!([{"status":"pass"},{"status":"pending"}])] {
        assert!(maho_omo_ulw_loop::index::status_has_active_incomplete_run(
            &serde_json::json!({"ok":true,"plan":{"goals":[{"status":"in_progress","successCriteria":criteria}]}})));
    }
}
