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
async fn runtime_preparation_loads_project_settings_applies_language_overrides_and_artifacts() {
    use maho_codemode::{interpreters::detect::InterpreterDetector,tool::types::{EvalLanguage,EvalRuntimeInfo}};
    let root=tempfile::tempdir().unwrap();
    let cwd=root.path().join("project");
    let home=root.path().join("home");
    std::fs::create_dir_all(cwd.join(".maho")).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::write(cwd.join(".maho/codemode.json"),r#"{"languages":{"py":false,"js":false,"rb":false,"jl":false},"parallelPoolWidth":3.9}"#).unwrap();
    let environment=std::collections::HashMap::from([("SENPI_CODEMODE_JS".into(),"true".into())]);
    let session_file=root.path().join("session.jsonl");
    let event=serde_json::json!({"sessionId":"runtime-session"});
    let mut detector=InterpreterDetector::new("24.0.0".into(),false);
    let runtime=prepare_runtime(RuntimePreparationOptions {cwd:&cwd,home_dir:&home,environment:&environment,event:&event,session_file:Some(&session_file),js_runtime:EvalRuntimeInfo{name:"bun".into(),version:"1.4.0".into(),path:Some("/usr/bin/bun".into())}},&mut detector).await.unwrap();
    assert_eq!(runtime.session_id,"runtime-session");
    assert_eq!(runtime.cwd,cwd);
    assert_eq!(runtime.parallel_pool_width,3);
    assert!(runtime.enabled_languages.js);
    assert!(!runtime.enabled_languages.py);
    assert!(!runtime.enabled_languages.rb);
    assert!(!runtime.enabled_languages.jl);
    assert!(runtime.settings.languages.js);
    assert_eq!(runtime.artifacts.dir,root.path().join("session-artifacts"));
    assert!(!runtime.artifacts.temp);
    assert!(runtime.artifacts.dir.is_dir());
    assert_eq!(runtime.runtimes.len(),1);
    assert_eq!(runtime.runtimes[0].0,EvalLanguage::Js);
    assert_eq!(runtime.runtimes[0].1.name,"bun");
    let executor=Arc::new(RuntimeExecuteTool {api:Arc::new(api()),active_tools:vec!["task".into()]});
    let session=create_runtime_with_factory(runtime,RuntimeHostOptions {executor:executor.clone(),active_tools:vec!["task".into()],list_tools:None,complete:Arc::new(|request|Box::pin(async move {Ok(serde_json::json!({"text":request.prompt}))})),session_env:std::collections::HashMap::from([("PI_SESSION_ID".into(),"context-session".into())])},|options| {
        assert_eq!(options.session_id,"runtime-session");
        assert_eq!(options.cwd,cwd);
        assert_eq!(options.session_env.as_ref().unwrap()["PI_SESSION_ID"],"context-session");
        assert_eq!(options.artifacts_dir.as_ref().unwrap(),&root.path().join("session-artifacts"));
        maho_codemode::extension::session_manager::CodemodeSessionManager::start(options)
    }).await.unwrap();
    assert!(session.spawns);
    assert_eq!(session.session_id,"runtime-session");
    assert_eq!(session.parallel_pool_width,3);
    assert_eq!(session.artifacts_dir,root.path().join("session-artifacts"));
    let port=session.manager.bridge_endpoint().unwrap().0;
    session.manager.dispose().await.unwrap();
    assert!(tokio::net::TcpStream::connect(("127.0.0.1",port)).await.is_err());
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
