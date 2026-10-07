//! Registered config-watch reload driver.
//!
//! Every case goes through the *published* registration path, never `ConfigWatchComponent::validate_changed_paths`:
//! the component publishes through `ExtensionApi::register_config_watch` -> `EventBus::publish_config_watch`
//! (typed registration on `config-watch:register` plus the JSON companion), and the driver reads the
//! registration back from the shared typed registry `EventBus::config_watch_registrations()` and invokes the
//! `validate` callable that registration carries. That callable is the one the real host consumer
//! (`maho-ext-config-reload::WatchRegistrations::register_native` -> `validate_external_paths`) stores and calls.
//!
//! Scenarios are recorded in `authoring/config.md` BEFORE these edits; every command is UNRUN until the
//! global gate. Each case uses a private `tempfile` HOME removed on drop; no daemon, PTY or subprocess is
//! started.
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::*;
use maho_omo_config_watch::index::*;
use maho_omo_config_watch::paths::resolve_omo_config_watch_target_resolution;
use maho_omo_config_watch::validate::OmoConfigValidator;

struct Fixture { _root: tempfile::TempDir, home: PathBuf, project: PathBuf, env: BTreeMap<String, String> }

fn fixture() -> Result<Fixture, std::io::Error> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::create_dir_all(project.join(".omo"))?;
    let env = BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    Ok(Fixture { _root: root, home, project, env })
}

/// The production component with the REAL target resolver and validator bound to the injected HOME, so the
/// published registration carries the same watched targets a live session resolves - no hand-built targets.
fn real_component(fixture: &Fixture) -> ConfigWatchComponent {
    let cwd = fixture.project.to_string_lossy().into_owned();
    let home = fixture.home.clone();
    let targets_env = fixture.env.clone();
    let validator_env = fixture.env.clone();
    let mut component = ConfigWatchComponent::default();
    component.options = ConfigWatchComponentOptions {
        resolve_cwd: Some(Arc::new(move || cwd.clone())),
        resolve_target_resolution: Some(Arc::new(move |cwd: &str| {
            resolve_omo_config_watch_target_resolution(Path::new(cwd), &home, &targets_env)
        })),
        create_validator: Some(Arc::new(move |cwd: &str| OmoConfigValidator::new(cwd.to_owned(), validator_env.clone()))),
        log: Some(Arc::new(|_, _, _| {})),
        ..Default::default()
    };
    component
}

fn api() -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("omo-owner", "/fixture".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default())
}

/// Acquire the registration the way a consumer does: from the shared typed registry, not by constructing it.
fn published(api: &ExtensionApi) -> RegisteredConfigWatch {
    let registrations = api.events.config_watch_registrations();
    assert_eq!(registrations.len(), 1, "exactly one published config-watch registration");
    let (owner, registration) = &registrations[0];
    assert_eq!(owner, "omo-owner");
    assert_eq!(registration.id, "omo");
    registration.clone()
}

// Case W1 - the published registration carries the real resolved targets and the upstream wire payload.
#[test]
fn published_registration_carries_the_real_resolved_targets() -> Result<(), std::io::Error> {
    let fixture = fixture()?;
    let component = real_component(&fixture);
    let mut api = api();
    let wire = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&wire);
    let _subscription = api.events.on(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |payload| {
        captured.lock().unwrap_or_else(PoisonError::into_inner).push(payload.clone());
    }));
    component.register(&mut api);
    let registration = published(&api);
    let expected = resolve_omo_config_watch_target_resolution(&fixture.project, &fixture.home, &fixture.env);
    let published_targets: Vec<PathBuf> = registration.targets.iter().map(|target| target.path.clone()).collect();
    let resolved_targets: Vec<PathBuf> = expected.targets.iter().map(|target| target.path.clone()).collect();
    assert_eq!(published_targets, resolved_targets, "published targets are the real resolved ones");
    assert!(!published_targets.is_empty(), "the resolver returns at least the user config directory");
    let payloads = wire.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(payloads.iter().any(|payload| *payload == config_watch_registration_json(&registration)),
        "the consumer's JSON parse input must be published alongside the typed registration");
    Ok(())
}

// Case W2/W3 - reject then repair, driven through the published validate callable.
#[test]
fn registry_validate_callback_rejects_then_repairs_the_reload() -> Result<(), std::io::Error> {
    let fixture = fixture()?;
    let config = fixture.project.join(".omo/omo.jsonc");
    std::fs::write(&config, r#"{"task":{"default_concurrency":3}}"#)?;
    let component = real_component(&fixture);
    let mut api = api();
    component.register(&mut api);
    let registration = published(&api);
    let changed = [config.clone()];
    std::fs::write(&config, r#"{"task":{"default_concurrency":"three"}}"#)?;
    let ConfigWatchValidation::Rejected { errors } = (registration.validate)(&changed) else {
        panic!("an invalid config must be rejected through the published callable");
    };
    assert!(!errors.is_empty());
    std::fs::write(&config, r#"{"task":{"default_concurrency":4}}"#)?;
    assert_eq!((registration.validate)(&changed), ConfigWatchValidation::Ok);
    Ok(())
}

// Case W4 - shutdown retirement: READY must not re-publish after the shutdown handler runs.
#[tokio::test]
async fn shutdown_retires_ready_republication() -> Result<(), std::io::Error> {
    let fixture = fixture()?;
    let component = real_component(&fixture);
    let mut api = api();
    let emissions = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&emissions);
    let _subscription = api.events.on_native::<(String, RegisteredConfigWatch)>(CONFIG_WATCH_REGISTER_CHANNEL, Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    component.register(&mut api);
    assert_eq!(emissions.load(Ordering::SeqCst), 1, "the initial registration is published");
    api.events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    assert_eq!(emissions.load(Ordering::SeqCst), 2, "READY re-publishes the registration");
    let context = support::context();
    for handler in api.registered.handlers[&EventKind::SessionShutdown].clone() {
        let mut event = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Reload, target_session_file: None, signal: None });
        handler(&mut event, &context).await.expect("shutdown handler");
    }
    api.events.emit(CONFIG_WATCH_READY, &JsonValue::Null);
    assert_eq!(emissions.load(Ordering::SeqCst), 2, "READY must not re-publish after shutdown");
    Ok(())
}
