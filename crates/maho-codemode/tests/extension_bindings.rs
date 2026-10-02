use maho_codemode::extension::{runtime_factory::*, skill_contribution::*};
use maho_ext_api::*;
use std::sync::Arc;

fn api() -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("codemode", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default())
}

#[test]
fn registers_real_resource_event_handler() {
    let mut api = api();
    register_bun_skill_contribution(&mut api, Arc::new(|| None), "/tmp".into(), "/usr/bin/node".into());
    assert_eq!(api.registered.handlers[&EventKind::ResourcesDiscover].len(), 1);
}

#[test]
fn gates_on_kernel_version_not_installed_binary() {
    for version in [None, Some("1.3.9"), Some("bad"), Some("v1.4.0")] { assert!(!bun_version_supports_skill(version)); }
    for version in ["1.4.0", "1.5.1", "2.0.0", "1.4.0-canary"] { assert!(bun_version_supports_skill(Some(version))); }
}

#[test]
fn resolves_shipped_skill_only_for_bun_kernel() {
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("skill/bun-1-4/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, "fixture").unwrap();
    let base = root.path().join("extension");
    std::fs::create_dir(&base).unwrap();
    assert!(active_bun_skill_path(None, &base, Path::new("/usr/bin/node")).is_none());
    assert!(active_bun_skill_path(Some("1.4.0"), &base, Path::new("/usr/bin/bun")).unwrap().exists());
}

use std::path::Path;
#[tokio::test]
async fn unbound_execute_tool_propagates_real_api_error() {
    let api = api();
    let error = execute_tool(&api, "task", JsonValue::Null, ExecuteToolOptions::default()).await.unwrap_err();
    assert_eq!(error.code, ExecuteToolErrorCode::Blocked);
    assert_eq!(error.tool_name, "task");
}

#[test]
fn active_tool_snapshot_does_not_require_bound_runtime() {
    let api = api();
    assert!(is_tool_available(&api, "task", Some(&["task".into()])).unwrap());
    assert!(!is_tool_available(&api, "other", Some(&["task".into()])).unwrap());
    assert!(is_tool_available(&api, "task", None).is_err());
}

#[test]
fn runtime_languages_require_settings_and_detection_not_availability_enabled() {
    use maho_codemode::{config::settings::{CodemodeSettings, Languages}, interpreters::detect::{InterpreterDetection, LanguageAvailability}, tool::types::EvalLanguage};
    let settings=CodemodeSettings {languages:Languages {py:true,js:true,rb:false,jl:true},..Default::default()};
    let availability=[EvalLanguage::Py,EvalLanguage::Js,EvalLanguage::Rb,EvalLanguage::Jl].map(|language|(language,LanguageAvailability {enabled:false,detected:if language==EvalLanguage::Jl {InterpreterDetection::Unavailable}else {InterpreterDetection::Detected {path:"interpreter".into(),version:"1.0".into(),resolved_path:None}}}));
    assert_eq!(enabled_languages_from(&settings,&availability),Languages {py:true,js:true,rb:false,jl:false});
}

#[test]
fn runtime_session_id_preserves_strings_and_generates_uuid_for_other_values() {
    assert_eq!(session_id_from(&serde_json::json!({"sessionId":""})),"");
    assert_eq!(session_id_from(&serde_json::json!({"sessionId":"session-1"})),"session-1");
    for event in [serde_json::Value::Null,serde_json::json!({"sessionId":42})] {
        let id=session_id_from(&event);
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(),4);
    }
}

#[tokio::test]
async fn runtime_executor_keeps_active_snapshot_and_propagates_native_dispatch_errors() {
    use maho_codemode::bridges::output_bridge::OutputExecuteTool;
    let executor=RuntimeExecuteTool {api:Arc::new(api()),active_tools:vec!["task".into()]};
    assert_eq!(executor.is_tool_available("task"),Some(true));
    assert_eq!(executor.is_tool_available("read"),Some(false));
    let error=executor.execute_tool("task",JsonValue::Null,ExecuteToolOptions::default()).await.unwrap_err();
    assert_eq!(error.code,ExecuteToolErrorCode::Blocked);
    assert_eq!(error.tool_name,"task");
}

#[test]
fn wake_snapshot_reaches_bus_and_rpc() {
    use maho_codemode::extension::wake_source_state::*;
    let api = api();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let bus_seen = Arc::clone(&seen);
    let _bus = api.events.on(WAKE_SOURCE_STATE_EVENT, Arc::new(move |data| bus_seen.lock().unwrap().push(data.clone())));
    let rpc_seen = Arc::clone(&seen);
    let _rpc = api.events.on("senpi:extension-rpc-event", Arc::new(move |data| rpc_seen.lock().unwrap().push(data.clone())));
    let state = WakeSourceState { source: SENPI_CODEMODE_WAKE_SOURCE.into(), active_count: 1, items: Some(vec![WakeSourceStateItem { id: "cell-1".into(), description: "running".into(), started_at_ms: 10.0 }]) };
    emit_wake_source_state(&api, &state).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["name"], WAKE_SOURCE_STATE_EVENT);
    assert_eq!(seen[0]["data"], seen[1]);
    assert_eq!(seen[1]["activeCount"], 1);
}
