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
