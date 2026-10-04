mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::*;
use maho_omo_config_watch::index::*;
use maho_omo_config_watch::paths::{OmoConfigWatchTarget, OmoConfigWatchTargetResolution};
use maho_omo_config_watch::validate::OmoConfigValidator;

type Logs = Arc<Mutex<Vec<(ConfigWatchLogLevel, String, Option<JsonValue>)>>>;

fn logs() -> Logs { Arc::new(Mutex::new(Vec::new())) }
fn sink(logs: &Logs) -> ConfigWatchLogSink {
    let logs = Arc::clone(logs);
    Arc::new(move |level, message, details| logs.lock().unwrap_or_else(PoisonError::into_inner).push((level, message.to_owned(), details.cloned())))
}
fn entries(logs: &Logs) -> Vec<(ConfigWatchLogLevel, String, Option<JsonValue>)> {
    logs.lock().unwrap_or_else(PoisonError::into_inner).clone()
}
fn api() -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("omo-owner", "/fixture".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default())
}
fn target(path: &str, globs: &[&str]) -> OmoConfigWatchTarget {
    OmoConfigWatchTarget { path: PathBuf::from(path), kind: "dir", filter_globs: globs.iter().map(|glob| (*glob).to_owned()).collect() }
}
fn make(options: ConfigWatchComponentOptions) -> ConfigWatchComponent {
    let mut component = ConfigWatchComponent::default();
    component.options = options;
    component
}
fn validator(cwd: &str) -> OmoConfigValidator {
    OmoConfigValidator::with_loader(cwd.to_owned(), BTreeMap::new(), Box::new(|_, _| Vec::new()))
}
fn watched(targets: Vec<OmoConfigWatchTarget>, logs: &Logs) -> ConfigWatchComponentOptions {
    ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(|| "/project".into())),
        resolve_targets: Some(Arc::new(move |_| targets.clone())),
        create_validator: Some(Arc::new(|cwd| validator(cwd))),
        log: Some(sink(logs)),
        ..Default::default()
    }
}

#[test]
fn emits_one_wire_valid_omo_registration_immediately() {
    let mut api = api();
    let logs = logs();
    let comp = make(watched(vec![target("/project/.omo", &["/omo.jsonc", "/omo.json"])], &logs));
    comp.register(&mut api);
    let registrations = api.events.config_watch_registrations();
    assert_eq!(registrations.len(), 1);
    let (owner, registration) = &registrations[0];
    assert_eq!(owner, "omo-owner");
    assert_eq!(registration.id, "omo");
    assert_eq!(registration.display_name, ".omo config");
    assert_eq!(registration.targets.len(), 1);
    assert_eq!(registration.targets[0].path, PathBuf::from("/project/.omo"));
    assert_eq!(registration.targets[0].kind, ConfigWatchTargetKind::Dir);
    assert_eq!(registration.targets[0].filter_globs, vec!["/omo.jsonc".to_owned(), "/omo.json".to_owned()]);
    assert_eq!(config_watch_registration_json(registration), serde_json::json!({
        "id": "omo",
        "displayName": ".omo config",
        "targets": [{ "path": "/project/.omo", "kind": "dir", "filterGlobs": ["/omo.jsonc", "/omo.json"] }],
    }));
}

#[test]
fn registers_with_minimal_capabilities_because_the_shared_bus_is_mandatory() {
    let mut api = api();
    let logs = logs();
    let comp = make(ConfigWatchComponentOptions { log: Some(sink(&logs)), ..Default::default() });
    comp.register(&mut api);
    assert_eq!(api.events.config_watch_registrations().len(), 1);
    assert_eq!(api.events.config_watch_registrations()[0].1.id, "omo");
    assert!(entries(&logs).iter().all(|(level, _, _)| *level != ConfigWatchLogLevel::Error));
}

#[test]
fn warns_when_user_config_creation_requires_reload() {
    let mut api = api();
    let logs = logs();
    let comp = make(ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(|| "/project".into())),
        resolve_target_resolution: Some(Arc::new(|_| OmoConfigWatchTargetResolution { targets: Vec::new(), user_config_creation_watched: false, user_config_creation_discovery: "reload_required" })),
        create_validator: Some(Arc::new(|cwd| validator(cwd))),
        log: Some(sink(&logs)),
        ..Default::default()
    });
    comp.register(&mut api);
    let entries = entries(&logs);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, ConfigWatchLogLevel::Warn);
    assert_eq!(entries[0].2, Some(serde_json::json!({ "userConfigCreationDiscovery": "reload_required" })));
}

#[test]
fn logs_reload_and_rejection_outcomes_with_paths_and_errors() {
    let mut api = api();
    let logs = logs();
    let events = api.events.clone();
    let comp = make(watched(vec![target("/project/.omo", &["/omo.jsonc"])], &logs));
    comp.register(&mut api);
    events.emit(CONFIG_WATCH_RELOADED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project/.omo/omo.jsonc"] }));
    events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project/.omo/omo.jsonc"], "errors": ["invalid config"] }));
    let entries = entries(&logs);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, ConfigWatchLogLevel::Info);
    assert_eq!(entries[0].2, Some(serde_json::json!({ "paths": ["/project/.omo/omo.jsonc"], "pathCount": 1 })));
    assert_eq!(entries[1].0, ConfigWatchLogLevel::Warn);
    assert_eq!(entries[1].2, Some(serde_json::json!({ "paths": ["/project/.omo/omo.jsonc"], "pathCount": 1, "errors": ["invalid config"], "errorCount": 1 })));
}

#[tokio::test]
async fn refreshes_targets_after_rejection_without_replacing_the_sticky_validator() {
    let mut api = api();
    let logs = logs();
    let passes = Arc::new(AtomicUsize::new(0));
    let resolved = Arc::clone(&passes);
    let comp = make(ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(|| "/project".into())),
        resolve_targets: Some(Arc::new(move |_| match resolved.fetch_add(1, Ordering::SeqCst) {
            0 => vec![target("/project", &["/.omo"])],
            _ => vec![target("/project", &["/.omo"]), target("/project/.omo", &["/omo.jsonc", "/omo.json"])],
        })),
        create_validator: Some(Arc::new(|cwd| validator(cwd))),
        log: Some(sink(&logs)),
        ..Default::default()
    });
    comp.register(&mut api);
    let before = api.events.config_watch_registrations()[0].1.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = api.events.on_native::<(String, RegisteredConfigWatch)>(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |registration| { let _ = sender.send(registration.clone()); }));
    api.events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project/.omo"], "errors": ["invalid config"] }));
    assert_eq!(api.events.config_watch_registrations()[0].1.targets.len(), 1);
    let retry = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.expect("bounded deferred re-registration").expect("retry emitted");
    assert_eq!(retry.1.targets.len(), 2);
    let after = api.events.config_watch_registrations()[0].1.clone();
    assert_eq!(after.targets.len(), 2);
    assert!(Arc::ptr_eq(&before.validate, &after.validate));
}

#[tokio::test]
async fn caps_deferred_reregistration_retries_when_the_host_rejects_deterministically() {
    let mut api = api();
    let logs = logs();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let reject_events = api.events.clone();
    let _subscription = api.events.on_native::<(String, RegisteredConfigWatch)>(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |registration| {
        let _ = sender.send(registration.clone());
        reject_events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project"], "errors": ["watch target covers protected senpi agent paths"] }));
    }));
    let comp = make(watched(vec![target("/project", &["/.omo"])], &logs));
    comp.register(&mut api);
    for expected in 1..=4usize {
        let received = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap_or_else(|_| panic!("bounded emission {expected}"));
        assert!(received.is_some(), "emission {expected}");
    }
    assert!(receiver.try_recv().is_err(), "the retry budget must stop after the cap");
    assert_eq!(entries(&logs).iter().filter(|(_, _, details)| details.as_ref().is_some_and(|value| value.get("maxRejectionRetries").and_then(serde_json::Value::as_u64) == Some(3))).count(), 1);
}

#[tokio::test]
async fn resets_the_rejection_retry_budget_when_the_registration_payload_changes() {
    let mut api = api();
    let logs = logs();
    let version = Arc::new(AtomicUsize::new(1));
    let resolved = Arc::clone(&version);
    let emissions = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&emissions);
    let events = api.events.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = api.events.on_native::<(String, RegisteredConfigWatch)>(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |registration| {
        counted.fetch_add(1, Ordering::SeqCst);
        let _ = sender.send(registration.clone());
    }));
    let comp = make(ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(|| "/project".into())),
        resolve_targets: Some(Arc::new(move |_| vec![target(&format!("/project/v{}", resolved.load(Ordering::SeqCst)), &["/.omo"])])),
        create_validator: Some(Arc::new(|cwd| validator(cwd))),
        log: Some(sink(&logs)),
        ..Default::default()
    });
    comp.register(&mut api);
    let _initial = receiver.recv().await.expect("initial emission");
    for expected in [2usize, 3, 4] {
        events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project"], "errors": ["invalid config"] }));
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.expect("bounded retry").expect("retry emitted");
        assert_eq!(emissions.load(Ordering::SeqCst), expected);
    }
    events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project"], "errors": ["invalid config"] }));
    assert!(receiver.try_recv().is_err(), "the v1 payload budget is exhausted");
    version.store(2, Ordering::SeqCst);
    events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({ "registrationId": "omo", "paths": ["/project"], "errors": ["invalid config"] }));
    let changed = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.expect("bounded changed-payload retry").expect("changed payload retries");
    assert_eq!(emissions.load(Ordering::SeqCst), 5);
    assert_eq!(changed.1.targets[0].path, PathBuf::from("/project/v2"));
}

#[tokio::test]
async fn releases_subscriptions_on_shutdown_and_replaces_them_on_repeated_register() {
    let mut api = api();
    let events = api.events.clone();
    let logs = logs();
    let emissions = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&emissions);
    let _subscription = events.on(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |_| { counted.fetch_add(1, Ordering::SeqCst); }));
    let comp = make(watched(vec![target("/project/.omo", &["/omo.jsonc"])], &logs));
    comp.register(&mut api);
    assert_eq!(emissions.load(Ordering::SeqCst), 1);
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    assert_eq!(emissions.load(Ordering::SeqCst), 2);
    comp.register(&mut api);
    assert_eq!(emissions.load(Ordering::SeqCst), 3);
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    assert_eq!(emissions.load(Ordering::SeqCst), 4);
    let mut event = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Reload, target_session_file: None, signal: None });
    let context = support::context();
    for handler in api.registered.handlers[&EventKind::SessionShutdown].clone() {
        handler(&mut event, &context).await.expect("shutdown");
    }
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    assert_eq!(emissions.load(Ordering::SeqCst), 4);
}

#[test]
fn keeps_one_active_registration_when_ready_is_emitted_repeatedly() {
    let mut api = api();
    let events = api.events.clone();
    let logs = logs();
    let comp = make(watched(vec![target("/project/.omo", &["/omo.jsonc"])], &logs));
    comp.register(&mut api);
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    let registrations = api.events.config_watch_registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].1.id, "omo");
}
