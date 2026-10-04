use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use maho_ext_api::*;
use maho_omo_config_watch::{index::*, paths::OmoConfigWatchTarget, validate::OmoConfigValidator};

#[tokio::test]
async fn ready_and_retry_publish_same_callable_with_refreshed_targets() {
    let events = EventBus::default();
    let targets = Arc::new(Mutex::new(vec![]));
    let resolved_targets = Arc::clone(&targets);
    let mut component = ConfigWatchComponent::default();
    component.options = ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(|| "/fixture".into())),
        resolve_targets: Some(Arc::new(move |_| resolved_targets.lock().unwrap().clone())),
        create_validator: Some(Arc::new(|cwd| OmoConfigValidator::with_loader(cwd.into(), BTreeMap::from([("HOME".into(), "/fixture".into())]), Box::new(|_, _| vec![])))),
        log: Some(Arc::new(|_, _, _| {})),
        ..Default::default()
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = events.on_native::<(String, RegisteredConfigWatch)>(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |registration| sender.send(registration.clone()).unwrap()));
    let mut api = ExtensionApi::new(LoadedExtension::new("omo-owner", "/fixture".into(), SourceInfo::default()), ExtensionSessionProfile::default(), events.clone(), ExtensionRuntime::default());
    component.register(&mut api);
    let initial = receiver.try_recv().unwrap();
    events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    let ready = receiver.try_recv().unwrap();
    assert!(Arc::ptr_eq(&initial.1.validate, &ready.1.validate));
    targets.lock().unwrap().push(OmoConfigWatchTarget { path: "/fixture/.omo".into(), kind: "dir", filter_globs: vec!["/omo.jsonc".into()] });
    events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({"registrationId":"omo","paths":[],"errors":["repair"]}));
    let retry = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
    assert_eq!(retry.0, "omo-owner");
    assert_eq!(retry.1.targets.len(), 1);
    assert!(Arc::ptr_eq(&initial.1.validate, &retry.1.validate));
    assert_eq!((retry.1.validate)(&[]), ConfigWatchValidation::Ok);
    assert_eq!(events.config_watch_registrations()[0].1.targets.len(), 1);
}
