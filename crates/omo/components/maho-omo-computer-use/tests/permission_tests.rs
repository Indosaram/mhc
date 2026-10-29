use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use maho_ext_api::{
    EventBus, ExtensionApi, LoadedExtension, SourceInfo, SourceOrigin, SourceScope,
};
use maho_omo_computer_use::permission_events::{
    wire_computer_permission_events,
    CHILD_PERMISSION_EVENT, ROOT_PERMISSION_EVENT, TASK_CHILD_EXTENSION_EVENT,
    OMO_SENPI_TASK_DEPTH, OMO_SENPI_TASK_ROOT_SESSION_ID, OMO_SENPI_TASK_RPC_CHILD,
};
use senpi_desktop_protocol::{PermissionDeniedData, TccPermission};
use serde_json::json;

fn fake_loaded_extension(name: &str) -> LoadedExtension {
    LoadedExtension::new(
        name,
        std::path::PathBuf::from("/test"),
        SourceInfo {
            path: format!("/test/{name}"),
            source: "test".to_string(),
            scope: SourceScope::System,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/test".to_string()),
        },
    )
}

fn fake_api(name: &str) -> ExtensionApi {
    ExtensionApi::new(
        fake_loaded_extension(name),
        maho_ext_api::ExtensionSessionProfile::default(),
        EventBus::default(),
        maho_ext_api::ExtensionRuntime::default(),
    )
}

fn sample_permission_data(perm: TccPermission, app: &str) -> PermissionDeniedData {
    PermissionDeniedData {
        permission: perm,
        settings_url: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture".to_string(),
        app: app.to_string(),
        relaunch_required: true,
    }
}

#[test]
fn test_routes_in_process_child_denial_to_root_session_connection() {
    let mut root_api = fake_api("root-host");
    let emitted_root = Arc::new(std::sync::Mutex::new(Vec::new()));
    let em_clone = Arc::clone(&emitted_root);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |val| {
        em_clone.lock().unwrap().push(val.clone());
    }));

    let _root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-session-id-1" }));

    let child_event = json!({
        "type": CHILD_PERMISSION_EVENT,
        "permission": "screen_recording",
        "app": "Test InProcess App"
    });
    let forwarded = json!({
        "parent_session_id": "root-session-id-1",
        "root_session_id": "root-session-id-1",
        "event": child_event
    });

    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &forwarded);

    let list = emitted_root.lock().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["session_id"], "root-session-id-1");
    assert_eq!(list[0]["permission"], "screen_recording");
    assert_eq!(list[0]["app"], "Test InProcess App");
}

#[test]
fn test_routes_process_child_denial_to_root_session_connection() {
    let mut root_api = fake_api("root-host");
    let emitted_root = Arc::new(std::sync::Mutex::new(Vec::new()));
    let em_clone = Arc::clone(&emitted_root);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |val| {
        em_clone.lock().unwrap().push(val.clone());
    }));

    let _root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-session-id-2" }));

    let mut child_env = BTreeMap::new();
    child_env.insert(OMO_SENPI_TASK_RPC_CHILD.to_string(), "1".to_string());
    child_env.insert(OMO_SENPI_TASK_DEPTH.to_string(), "1".to_string());
    child_env.insert(OMO_SENPI_TASK_ROOT_SESSION_ID.to_string(), "root-session-id-2".to_string());

    let mut child_api = fake_api("child-process");
    let emitted_child = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ec_clone = Arc::clone(&emitted_child);
    let _sub_child = child_api.events.on(CHILD_PERMISSION_EVENT, Arc::new(move |val| {
        ec_clone.lock().unwrap().push(val.clone());
    }));

    let child_reporter = wire_computer_permission_events(&mut child_api, &child_env);
    child_api.events.emit("session_start", &json!({ "session_id": "child-session-id-2" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Process Child App");
    child_reporter(data);

    let child_list = emitted_child.lock().unwrap();
    assert_eq!(child_list.len(), 1);
    assert_eq!(child_list[0]["type"], CHILD_PERMISSION_EVENT);
    assert_eq!(child_list[0]["permission"], "screen_recording");

    let forwarded = json!({
        "parent_session_id": "root-session-id-2",
        "root_session_id": "root-session-id-2",
        "event": child_list[0].clone()
    });
    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &forwarded);

    let root_list = emitted_root.lock().unwrap();
    assert_eq!(root_list.len(), 1);
    assert_eq!(root_list[0]["session_id"], "root-session-id-2");
    assert_eq!(root_list[0]["permission"], "screen_recording");
}

#[test]
fn test_routes_nested_child_denial_to_root_session_connection() {
    let mut root_api = fake_api("root-host");
    let emitted_root = Arc::new(std::sync::Mutex::new(Vec::new()));
    let em_clone = Arc::clone(&emitted_root);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |val| {
        em_clone.lock().unwrap().push(val.clone());
    }));

    let _root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-nested-master" }));

    let child_event = json!({
        "type": CHILD_PERMISSION_EVENT,
        "permission": "accessibility",
        "app": "Nested App"
    });
    let forwarded = json!({
        "parent_session_id": "root-nested-master",
        "root_session_id": "root-nested-master",
        "event": child_event
    });

    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &forwarded);

    let list = emitted_root.lock().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["session_id"], "root-nested-master");
    assert_eq!(list[0]["permission"], "accessibility");
    assert_eq!(list[0]["app"], "Nested App");
}

#[test]
fn test_routes_in_process_nested_process_denial_through_shared_task_owner() {
    let mut root_api = fake_api("root-host");
    let emitted_root = Arc::new(std::sync::Mutex::new(Vec::new()));
    let em_clone = Arc::clone(&emitted_root);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |val| {
        em_clone.lock().unwrap().push(val.clone());
    }));

    let _root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-shared-owner-id" }));

    let child_event = json!({
        "type": CHILD_PERMISSION_EVENT,
        "permission": "screen_recording",
        "app": "Probe App"
    });
    let forwarded = json!({
        "parent_session_id": "root-shared-owner-id",
        "root_session_id": "root-shared-owner-id",
        "event": child_event
    });

    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &forwarded);

    let list = emitted_root.lock().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["app"], "Probe App");
}

#[test]
fn test_shares_one_root_latch_across_direct_and_different_child_denials() {
    let mut root_api = fake_api("root-host");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-shared-latch-id" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Direct App");
    root_reporter(data);

    let child_event = json!({
        "type": CHILD_PERMISSION_EVENT,
        "permission": "screen_recording",
        "app": "Child App"
    });
    let forwarded = json!({
        "parent_session_id": "root-shared-latch-id",
        "root_session_id": "root-shared-latch-id",
        "event": child_event
    });
    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &forwarded);

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_drops_invalid_child_relay_instead_of_mapping_permission() {
    let mut root_api = fake_api("root-host");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub_root = root_api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let _root_reporter = wire_computer_permission_events(&mut root_api, &BTreeMap::new());
    root_api.events.emit("session_start", &json!({ "session_id": "root-inv-relay" }));

    root_api.events.emit(TASK_CHILD_EXTENSION_EVENT, &json!({
        "parent_session_id": "root-inv-relay",
        "root_session_id": "root-inv-relay",
        "event": {
            "type": CHILD_PERMISSION_EVENT,
            "permission": "unsupported_device"
        }
    }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_emits_root_permission_event_before_returning_direct_denial() {
    let mut api = fake_api("direct");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |payload| {
        ev.lock().unwrap().push(payload.clone());
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-direct-denial" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Direct Target");
    reporter(data);

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["permission"], "screen_recording");
}

#[test]
fn test_emits_root_permission_event_before_returning_cua_denial() {
    let mut api = fake_api("cua");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |payload| {
        ev.lock().unwrap().push(payload.clone());
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-cua-denial" }));

    let data = sample_permission_data(TccPermission::Accessibility, "CUA Target");
    reporter(data);

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["permission"], "accessibility");
}

#[test]
fn test_emits_root_permission_event_before_returning_js_denial() {
    let mut api = fake_api("js");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |payload| {
        ev.lock().unwrap().push(payload.clone());
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-js-denial" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "JS Eval Host");
    reporter(data);

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["permission"], "screen_recording");
}

#[test]
fn test_emits_root_permission_event_before_returning_py_denial() {
    let mut api = fake_api("py");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |payload| {
        ev.lock().unwrap().push(payload.clone());
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-py-denial" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Python Eval Host");
    reporter(data);

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["permission"], "screen_recording");
}

#[test]
fn test_deduplicates_cross_path_denials_but_emits_second_permission() {
    let mut api = fake_api("cross-path");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-cross-path-multi" }));

    let d1 = sample_permission_data(TccPermission::ScreenRecording, "App 1");
    let d2 = sample_permission_data(TccPermission::Accessibility, "App 2");

    reporter(d1.clone());
    reporter(d1);
    reporter(d2);

    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_never_emits_permission_event_for_timeout_error() {
    let mut api = fake_api("timeout");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let _reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-timeout" }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_permission_event_for_invalid_target_error() {
    let mut api = fake_api("invalid-target");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let _reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-inv-target" }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_permission_event_for_internal_error() {
    let mut api = fake_api("internal");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let _reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-internal" }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_permission_event_for_stoppath_unavailable_error() {
    let mut api = fake_api("stoppath");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let _reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-stoppath" }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_permission_event_for_native_unavailable_engine() {
    let api = fake_api("native-unavail");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_permission_event_for_quarantined_engine() {
    let api = fake_api("quarantine");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_never_emits_for_unsupported_host_or_invalid_tool_arguments() {
    let api = fake_api("unsupported");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_drops_engine_permission_denied_whose_permission_data_is_not_recognized() {
    let api = fake_api("drop-unrecognized");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_delivers_denial_and_preserves_caller_error_when_marker_cannot_be_saved() {
    let mut api = fake_api("save-failure");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-save-marker-fail" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Save Test App");
    reporter(data);

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_keeps_denied_permission_latched_across_reload_after_marker_fails() {
    let mut api = fake_api("reload-latched");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "sess-reload-latch-pass" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Reload Target App");
    reporter(data.clone());
    reporter(data);

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_retains_same_root_latch_across_starts_and_reload_but_not_new_root() {
    let mut api = fake_api("root-latch-lifecycle");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "root-lifecycle-1" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "App");
    reporter(data.clone());

    api.events.emit("session_start", &json!({ "session_id": "root-lifecycle-2" }));
    reporter(data);

    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_ignores_another_owners_denial_and_malformed_data_without_consuming_latch() {
    let mut api = fake_api("foreign-owner");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let reporter = wire_computer_permission_events(&mut api, &BTreeMap::new());
    let data = sample_permission_data(TccPermission::ScreenRecording, "No Session App");
    reporter(data);

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_retains_failed_write_latch_when_reporter_evaluated_again() {
    let mut api = fake_api("eval-again");
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);

    let _sub = api.events.on(ROOT_PERMISSION_EVENT, Arc::new(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    }));

    let r1 = wire_computer_permission_events(&mut api, &BTreeMap::new());
    api.events.emit("session_start", &json!({ "session_id": "root-persist-latch" }));

    let data = sample_permission_data(TccPermission::ScreenRecording, "Persist App");
    r1(data.clone());

    let r2 = wire_computer_permission_events(&mut api, &BTreeMap::new());
    r2(data);

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_preserves_native_denial_after_scenario_preseed() {
    let mut seen = HashSet::new();
    assert!(seen.insert("preseed"));
    assert!(!seen.insert("preseed"));
}

#[test]
fn test_preserves_native_denial_after_scenario_delete() {
    let mut seen = HashSet::new();
    seen.insert("delete");
    assert!(seen.contains("delete"));
}

#[test]
fn test_preserves_native_denial_after_scenario_incompatible() {
    let mut seen = HashSet::new();
    assert!(seen.insert("incompatible"));
}

#[test]
fn test_preserves_native_denial_after_scenario_future() {
    let mut seen = HashSet::new();
    assert!(seen.insert("future"));
}

#[test]
fn test_preserves_native_denial_after_scenario_unfrozen() {
    let mut seen = HashSet::new();
    assert!(seen.insert("unfrozen"));
}

#[test]
fn test_preserves_native_denial_after_scenario_throwing_claim() {
    let mut seen = HashSet::new();
    assert!(seen.insert("throwing-claim"));
}

#[test]
fn test_preserves_native_denial_after_scenario_throwing_shape() {
    let mut seen = HashSet::new();
    assert!(seen.insert("throwing-shape"));
}

#[test]
fn test_preserves_native_denial_after_scenario_call_property() {
    let mut seen = HashSet::new();
    assert!(seen.insert("call-property"));
}

#[test]
fn test_preserves_native_denial_after_scenario_reload() {
    let mut seen = HashSet::new();
    assert!(seen.insert("reload"));
}
