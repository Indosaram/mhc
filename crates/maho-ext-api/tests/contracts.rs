use maho_ext_api::*;
use std::sync::{Arc, Mutex};

#[test]
fn every_event_kind_roundtrips_without_duplicate_names() {
    let names: std::collections::BTreeSet<_> = EventKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(names.len(), 43);
    for kind in EventKind::ALL { assert_eq!(EventKind::parse(kind.as_str()), Some(kind)); }
    assert_eq!(EventKind::parse("missing"), None);
}

#[test]
fn event_bus_subscription_drop_unsubscribes_and_keeps_order() {
    let bus = EventBus::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first_seen = Arc::clone(&seen);
    let first = bus.on("x", Arc::new(move |_| first_seen.lock().unwrap().push(1)));
    let second_seen = Arc::clone(&seen);
    let second = bus.on("x", Arc::new(move |_| second_seen.lock().unwrap().push(2)));
    bus.emit("x", &JsonValue::Null);
    drop(first);
    bus.emit("x", &JsonValue::Null);
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 2]);
    drop(second);
}

#[test]
fn flag_defaults_are_first_wins_and_cli_values_override() {
    let runtime = ExtensionRuntime::default();
    let mut api = ExtensionApi::new(LoadedExtension::new("first", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime.clone());
    api.register_flag("enabled", FlagType::Boolean { default: Some(true) }, None);
    api.register_flag("enabled", FlagType::Boolean { default: Some(false) }, None);
    assert_eq!(api.get_flag("enabled"), Some(FlagValue::Boolean(true)));
    api.set_flag("enabled", FlagValue::Boolean(false));
    assert_eq!(api.get_flag("enabled"), Some(FlagValue::Boolean(false)));
}

#[test]
fn runtime_invalidation_preserves_first_reason_and_rejects_actions() {
    let runtime = ExtensionRuntime::default();
    runtime.invalidate("replacement");
    runtime.invalidate("later");
    assert_eq!(runtime.assert_active().unwrap_err().message, "replacement");
}

fn api(runtime: ExtensionRuntime) -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("test", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime)
}
#[derive(Default)]
struct Providers(Mutex<Vec<String>>);
impl ExtensionProviderActions for Providers {
    fn register_provider(&self, registration: ProviderRegistration, path: &str) -> Result<(), ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("{}:{path}", registration.name())); Ok(())
    }
    fn unregister_provider(&self, name: &str, _: &str) -> Result<(), ExtensionFailure> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("remove:{name}")); Ok(())
    }
}
#[test]
fn providers_queue_in_order_then_register_immediately_after_binding() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("first", ProviderConfig::default()).unwrap();
    api.register_provider("removed", ProviderConfig::default()).unwrap();
    api.unregister_provider("removed").unwrap();
    api.register_provider("second", ProviderConfig::default()).unwrap();
    let providers = Arc::new(Providers::default()); runtime.bind_providers(providers.clone()).unwrap();
    api.register_provider("third", ProviderConfig::default()).unwrap(); api.unregister_provider("first").unwrap();
    assert_eq!(*providers.0.lock().unwrap(), ["first:test", "second:test", "third:test", "remove:first"]);
}
#[test]
fn invalidation_rejects_provider_changes_and_discards_pending_registrations() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("queued", ProviderConfig::default()).unwrap(); runtime.invalidate("old generation");
    assert_eq!(api.unregister_provider("queued").unwrap_err().message, "old generation");
    assert!(runtime.bind_providers(Arc::new(Providers::default())).is_err());
}
#[test]
fn read_classifiers_are_first_wins_and_unsubscribe_on_drop_or_invalidation() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    let make = |label: &'static str| -> ReadClassifier { Arc::new(move |_, _| Some(CompactReadClassification { kind: CompactReadKind::Docs, label: label.into(), headline: None })) };
    let first = api.register_read_classifier(make("first")).unwrap();
    let _second = api.register_read_classifier(make("second")).unwrap();
    assert_eq!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).unwrap().label, "first");
    drop(first);
    assert_eq!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).unwrap().label, "second");
    runtime.invalidate("reload"); assert!(runtime.classify_read(std::path::Path::new("/docs"), &api.cwd).is_none());
}
#[test]
fn classifier_panics_do_not_block_later_classifiers() {
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    let _bad = api.register_read_classifier(Arc::new(|_, _| panic!("classifier"))).unwrap();
    let _good = api.register_read_classifier(Arc::new(|_, _| Some(CompactReadClassification { kind: CompactReadKind::Memory, label: "memory".into(), headline: None }))).unwrap();
    assert_eq!(runtime.classify_read(std::path::Path::new("/memory"), &api.cwd).unwrap().kind, CompactReadKind::Memory);
}
#[test]
fn session_actions_fail_explicitly_before_host_binding() {
    let api = api(ExtensionRuntime::default());
    assert!(api.set_session_name("name").is_err()); assert!(api.get_session_name().is_err());
    assert!(api.get_active_tools().is_err()); assert!(api.set_active_tools(vec![]).is_err());
    assert!(api.get_commands().is_err()); assert!(api.get_thinking_level().is_err());
    assert!(api.set_session_fast_mode(true).is_err());
}

#[test]
fn failed_queued_provider_does_not_discard_later_registrations() {
    struct Selective(Providers);
    impl ExtensionProviderActions for Selective {
        fn register_provider(&self, registration: ProviderRegistration, path: &str) -> Result<(), ExtensionFailure> {
            if registration.name() == "bad" { return Err(ExtensionFailure::new("invalid provider")); }
            self.0.register_provider(registration, path)
        }
        fn unregister_provider(&self, name: &str, path: &str) -> Result<(), ExtensionFailure> { self.0.unregister_provider(name, path) }
    }
    let runtime = ExtensionRuntime::default(); let api = api(runtime.clone());
    api.register_provider("bad", ProviderConfig::default()).unwrap(); api.register_provider("good", ProviderConfig::default()).unwrap();
    let actions = Arc::new(Selective(Providers::default())); runtime.bind_providers(actions.clone()).unwrap();
    assert_eq!(*actions.0.0.lock().unwrap(), ["good:test"]);
    let errors = runtime.take_provider_errors(); assert_eq!(errors.len(), 1); assert_eq!(errors[0].extension_path, "test"); assert_eq!(errors[0].event, "register_provider");
    assert!(runtime.take_provider_errors().is_empty());
}
