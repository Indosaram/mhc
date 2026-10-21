mod facets_support;
use facets_support::*;
use maho_chord::{*, context::BACKGROUND_CONTEXT, facets::host::FacetServiceHandle, types::{ServiceError, ServiceImplementation}};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn activates_dependencies_before_consumers_and_disposes_in_reverse() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let descriptor = service("source", false);
    let retained = Arc::new(Mutex::new(None::<Arc<FacetServiceHandle>>)); let handle = retained.clone(); let log = trace.clone(); let dependency = descriptor.clone();
    let consumer = facet("consumer", move |env| {
        let value = env.use_service(&dependency)?.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        assert!(value.member(&"read".into()).value().is_err());
        let second = env.use_service(&dependency)?.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        assert!(Arc::ptr_eq(&value, &second)); *lock(&handle) = Some(value);
        env.on_activate(effect(log.clone(), "activate consumer", false))?;
        env.on_deactivate(effect(log.clone(), "dispose consumer", false))
    });
    let source = provider("source", descriptor, "A", trace.clone(), false, false);
    // When
    let host = create_facet_host(options(vec![consumer, source])).await?;
    let value = lock(&retained).clone().ok_or_else(failure)?;
    assert_eq!(value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("A".into())));
    host.dispose().await?;
    // Then
    assert_eq!(*lock(&trace), ["activate A", "activate consumer", "dispose consumer", "dispose A"]);
    assert!(value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err()); Ok(())
}

#[tokio::test]
async fn rejects_missing_dependencies_without_activation() {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let log = trace.clone();
    let consumer = facet("consumer", move |env| { env.use_service(&service("missing", false))?; env.on_activate(effect(log.clone(), "activate", false)) });
    // When
    let result = create_facet_host(options(vec![consumer])).await;
    // Then
    assert!(result.is_err()); assert!(lock(&trace).is_empty());
}

#[tokio::test]
async fn rejects_cycles_and_disposes_every_prepared_facet() {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let make = |id: &'static str, required: &'static str| { let log = trace.clone(); facet(id, move |env| { env.use_service(&service(required, false))?; env.provide(&service(id, false), implementation(id, false)?)?; env.on_deactivate(effect(log.clone(), id, false)) }) };
    // When
    let result = create_facet_host(options(vec![make("left", "right"), make("right", "left")])).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["right", "left"]);
}

#[tokio::test]
async fn rejects_duplicate_and_mode_conflicting_providers() {
    // Given
    let singleton = || facet("singleton", |env| env.provide(&service("value", false), implementation("A", false)?));
    let duplicate = facet("duplicate", |env| env.provide(&service("value", false), implementation("B", false)?));
    let keyed = facet("keyed", |env| { env.provide_many(&service("value", false))?; Ok(()) });
    // When
    let duplicates = create_facet_host(options(vec![singleton(), duplicate])).await;
    let modes = create_facet_host(options(vec![singleton(), keyed])).await;
    // Then
    assert!(duplicates.is_err()); assert!(modes.is_err());
}

#[tokio::test]
async fn cleans_resources_of_the_facet_whose_setup_fails() {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let log = trace.clone();
    let broken = facet("broken", move |env| { env.own(effect(log.clone(), "cleanup", false))?; Err(failure()) });
    // When
    let result = create_facet_host(options(vec![broken])).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["cleanup"]);
}

#[tokio::test]
async fn owns_resources_registered_during_activation() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let log = trace.clone();
    let owner = facet("owner", move |env| { let env = env.clone_environment(); let log = log.clone(); env.clone_environment().on_activate(Box::new(move || Box::pin(async move { env.own(effect(log, "cleanup", false)) }))) });
    let host = create_facet_host(options(vec![owner])).await?;
    // When
    host.dispose().await?; host.dispose().await?;
    // Then
    assert_eq!(*lock(&trace), ["cleanup"]); Ok(())
}

#[tokio::test]
async fn keyed_observations_receive_cancelled_contexts_on_retirement() -> Result<(), ServiceError> {
    // Given
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let observer = facet("observer", move |env| { let sender = sender.clone(); env.observe(&service("keyed", true), Arc::new(move |value, context| { let result = sender.send((value, context)).map_err(|_| failure()); Box::pin(async move { result }) })) });
    let provider = facet("provider", |env| { let spawner = env.provide_many(&service("keyed", true))?; let value = implementation("A", true)?; env.on_activate(Box::new(move || { let result = spawner.spawn(&"one".into(), value); Box::pin(async move { let _release = result?; Ok(()) }) })) });
    // When
    let host = create_facet_host(options(vec![observer, provider])).await?;
    let (value, context) = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    let value = value.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
    assert!(value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_ok());
    host.dispose().await?;
    // Then
    assert!(context.abort_signal().is_some_and(|s| s.aborted()));
    assert!(value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err()); Ok(())
}

#[tokio::test]
async fn reports_all_reverse_cleanup_failures() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let log = trace.clone();
    let owner = facet("owner", move |env| { env.own(effect(log.clone(), "first", true))?; env.own(effect(log.clone(), "second", true)) });
    let host = create_facet_host(options(vec![owner])).await?;
    // When
    let error = host.dispose().await.err().ok_or_else(failure)?;
    // Then
    let errors = error.downcast_ref::<maho_chord::services::errors::CollectedErrors>().ok_or_else(failure)?;
    assert_eq!(errors.errors.len(), 2); assert_eq!(*lock(&trace), ["second", "first"]); Ok(())
}

#[test]
fn service_definitions_validate_ids_and_preserve_locality() -> Result<(), ServiceError> {
    // Given
    let id = "local";
    // When
    let definition = define_service::<ServiceImplementation>(id, true)?;
    // Then
    assert_eq!(definition.descriptor(), service(id, true));
    assert!(define_service::<()>("", false).is_err()); assert!(define_service::<()>("$chord.control", false).is_err()); Ok(())
}

struct Source { provider: Arc<maho_chord::services::provider::RemoteServiceProvider>, opened: Arc<Mutex<Vec<Vec<JsString>>>>, deferred: bool }

struct ObjectSource {
    current: Arc<Mutex<(JsString, ServiceImplementation)>>,
    opened: Arc<Mutex<Vec<Vec<JsString>>>>,
    disposed: Arc<Mutex<usize>>,
}

#[test]
fn replicated_state_source_overloads_attach_and_dispose_independently() -> Result<(), ServiceError> {
    // Given
    struct NativeSource(Arc<Mutex<usize>>);
    struct Attachment { snapshot: maho_chord::types::ReplicatedStateSourceSnapshot<JsonValue>, disposed: Arc<Mutex<usize>> }
    impl maho_chord::types::ReplicatedStateSource<JsonValue> for NativeSource {
        fn attach(&self) -> Result<Box<dyn maho_chord::types::ReplicatedStateSourceAttachment<JsonValue>>, ServiceError> {
            Ok(Box::new(Attachment { snapshot: maho_chord::types::ReplicatedStateSourceSnapshot { value: Arc::new(JsonValue::String("snapshot".into())), cursor: 0 }, disposed: self.0.clone() }))
        }
    }
    impl maho_chord::types::ReplicatedStateSourceAttachment<JsonValue> for Attachment {
        fn snapshot(&self) -> &maho_chord::types::ReplicatedStateSourceSnapshot<JsonValue> { &self.snapshot }
        fn activate(&mut self, mut listener: Box<dyn FnMut(maho_chord::types::ReplicatedStateSourceFrame<JsonValue>) + Send>) -> Result<(), ServiceError> {
            listener(maho_chord::types::ReplicatedStateSourceFrame { cursor: 1, value: Arc::new(JsonValue::String("activated".into())), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
            Ok(())
        }
        fn dispose(&mut self) { *lock(&self.disposed) += 1; }
    }
    let disposed = Arc::new(Mutex::new(0)); let source = NativeSource(disposed.clone());
    let source: &dyn maho_chord::types::ReplicatedStateSource<JsonValue> = &source;
    // When
    let defaulted = replicated_state(source)?;
    let configured = replicated_state((source, maho_chord::types::ReplicatedStateSourceOptions { on_error: None }))?;
    // Then
    assert_eq!(defaulted.value().as_deref(), Some(&JsonValue::String("activated".into())));
    assert_eq!(configured.value().as_deref(), Some(&JsonValue::String("activated".into())));
    defaulted.dispose(); defaulted.dispose(); configured.dispose();
    assert_eq!(*lock(&disposed), 2);
    Ok(())
}
struct ObjectBinding { value: ServiceImplementation, disposed: Arc<Mutex<usize>> }
impl RemoteServices for ObjectBinding {
    fn use_service(&self, _: &JsString) -> Result<ServiceImplementation, ServiceError> { Ok(self.value.clone()) }
    fn observe(&self, _: &JsString, _: maho_chord::types::ServiceObserver) -> Result<maho_chord::types::Disposal, ServiceError> { Err(failure()) }
    fn ready(&self, _: maho_chord::types::ContextRef) -> maho_chord::types::BoxFuture<'_, Result<(), ServiceError>> { Box::pin(async { Ok(()) }) }
    fn dispose(&self, _: maho_chord::types::ContextRef) -> maho_chord::types::BoxFuture<'_, Result<(), ServiceError>> { Box::pin(async { *lock(&self.disposed) += 1; Ok(()) }) }
}
impl RemoteServiceSource for ObjectSource {
    fn accepts_unavailable_services(&self) -> bool { false }
    fn catalogue(&self, _: maho_chord::types::ContextRef) -> maho_chord::types::BoxFuture<'_, Result<Vec<ServiceCatalogueEntry>, ServiceError>> {
        Box::pin(async { Ok(vec![ServiceCatalogueEntry { service_id: lock(&self.current).0.clone(), mode: ServiceMode::Singleton }]) })
    }
    fn open(&self, services: &[JsString], _: maho_chord::services::handle::AccessGuard, _: maho_chord::types::ErrorHandler) -> Result<Arc<dyn RemoteServices>, ServiceError> {
        lock(&self.opened).push(services.to_vec());
        Ok(Arc::new(ObjectBinding { value: lock(&self.current).1.clone(), disposed: self.disposed.clone() }))
    }
}

#[tokio::test]
async fn reopens_changed_sources_and_accepts_explicit_singleton_objects() -> Result<(), ServiceError> {
    // Given
    let current = Arc::new(Mutex::new((JsString::from("left"), implementation("left value", true)?)));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let disposed = Arc::new(Mutex::new(0));
    let source: Arc<dyn RemoteServiceSource> = Arc::new(ObjectSource { current: current.clone(), opened: opened.clone(), disposed: disposed.clone() });
    let values = Arc::new(Mutex::new(Vec::new()));
    let retained = Arc::new(Mutex::new(Vec::<Arc<FacetServiceHandle>>::new()));
    let consumer = |id: &'static str| {
        let values = values.clone(); let retained = retained.clone();
        facet("consumer", move |env| {
            let value = env.use_service(&service(id, false))?.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
            lock(&retained).push(value.clone());
            let values = values.clone();
            env.on_activate(Box::new(move || Box::pin(async move {
                let result = value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await?;
                lock(&values).push(result); Ok(())
            })))
        })
    };
    let mut first_options = options(vec![consumer("left")]); first_options.service_sources.push(source.clone());
    let first = create_facet_host(first_options).await?;
    first.dispose().await?;
    *lock(&current) = ("right".into(), implementation("right value", true)?);
    let mut second_options = options(vec![consumer("right")]); second_options.service_sources.push(source);
    // When
    let second = create_facet_host(second_options).await?;
    // Then
    assert_eq!(*lock(&values), [Some(JsonValue::String("left value".into())), Some(JsonValue::String("right value".into()))]);
    assert_eq!(*lock(&opened), [vec![JsString::from("left")], vec![JsString::from("right")]]);
    let stale = lock(&retained)[0].clone();
    assert!(stale.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    second.dispose().await?;
    assert_eq!(*lock(&disposed), 2);
    Ok(())
}

#[tokio::test]
async fn retired_environment_rejects_setup_and_runtime_operations() -> Result<(), ServiceError> {
    // Given
    let retained = Arc::new(Mutex::new(None)); let saved = retained.clone();
    let owner = facet("owner", move |env| { *lock(&saved) = Some(env.clone_environment()); Ok(()) });
    let host = create_facet_host(options(vec![owner])).await?;
    let env = lock(&retained).clone().ok_or_else(failure)?;
    // When
    host.dispose().await?;
    // Then
    assert!(env.use_service(&service("missing", false)).is_err());
    assert!(env.provide(&service("late", true), implementation("late", true)?).is_err());
    assert!(env.provide_many(&service("late", true)).is_err());
    assert!(env.replicated_state(JsonValue::object(vec![])).is_err());
    assert!(env.own(Box::new(|| Box::pin(async { Ok(()) }))).is_err());
    assert!(env.on_activate(Box::new(|| Box::pin(async { Ok(()) }))).is_err());
    assert!(env.on_deactivate(Box::new(|| Box::pin(async { Ok(()) }))).is_err());
    Ok(())
}
impl RemoteServiceSource for Source {
    fn accepts_unavailable_services(&self) -> bool { self.deferred }
    fn catalogue(&self, _: maho_chord::types::ContextRef) -> maho_chord::types::BoxFuture<'_, Result<Vec<ServiceCatalogueEntry>, ServiceError>> { Box::pin(async { Ok(self.provider.catalogue().to_vec()) }) }
    fn open(&self, services: &[JsString], assert_access: maho_chord::services::handle::AccessGuard, on_error: maho_chord::types::ErrorHandler) -> Result<Arc<dyn RemoteServices>, ServiceError> {
        lock(&self.opened).push(services.to_vec());
        Ok(create_remote_service_binding(RemoteServiceBindingOptions { services: services.to_vec(), transport: self.provider.clone(), bound: true, on_error: Some(on_error), assert_access: Some(assert_access) })?)
    }
}
#[tokio::test]
async fn external_sources_are_grouped_and_available_before_activation() -> Result<(), ServiceError> {
    // Given
    let opened = Arc::new(Mutex::new(Vec::new()));
    let provider = maho_chord::services::provider::RemoteServiceProvider::new(vec![maho_chord::services::provider::ServiceProviderDefinition { service_id: "external".into(), local: false, mode: ServiceMode::Singleton }])?;
    provider.provide(&"external".into(), implementation("outside", false)?.downcast().map_err(|_| failure())?)?;
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let consumer = facet("consumer", move |env| {
        let value = env.use_service(&service("external", false))?.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        let sent = sent.clone(); env.on_activate(Box::new(move || Box::pin(async move { let value = value.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await?; sent.send(value).map_err(|_| failure()) })))
    });
    let mut configuration = options(vec![consumer]); configuration.service_sources.push(Arc::new(Source { provider, opened: opened.clone(), deferred: false }));
    // When
    let host = create_facet_host(configuration).await?;
    let value = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_eq!(value, Some(JsonValue::String("outside".into()))); assert_eq!(*lock(&opened), [vec![JsString::from("external")]]); host.dispose().await?; Ok(())
}

#[tokio::test]
async fn duplicate_external_offers_are_rejected_without_opening() -> Result<(), ServiceError> {
    // Given
    let opened = Arc::new(Mutex::new(Vec::new()));
    let provider = maho_chord::services::provider::RemoteServiceProvider::new(vec![maho_chord::services::provider::ServiceProviderDefinition { service_id: "external".into(), local: false, mode: ServiceMode::Singleton }])?;
    let source: Arc<dyn RemoteServiceSource> = Arc::new(Source { provider, opened: opened.clone(), deferred: false });
    let mut configuration = options(vec![]); configuration.service_sources = vec![source.clone(), source];
    // When
    let result = create_facet_host(configuration).await;
    // Then
    assert!(result.is_err()); assert!(lock(&opened).is_empty()); Ok(())
}

#[tokio::test]
async fn multiple_deferred_sources_are_rejected_without_opening() -> Result<(), ServiceError> {
    // Given
    let opened = Arc::new(Mutex::new(Vec::new()));
    let provider = maho_chord::services::provider::RemoteServiceProvider::new(vec![])?;
    let source: Arc<dyn RemoteServiceSource> = Arc::new(Source { provider, opened: opened.clone(), deferred: true });
    let consumer = facet("consumer", |env| { env.use_service(&service("deferred", false))?; Ok(()) });
    let mut configuration = options(vec![consumer]); configuration.service_sources = vec![source.clone(), source];
    // When
    let result = create_facet_host(configuration).await;
    // Then
    assert!(result.is_err()); assert!(lock(&opened).is_empty()); Ok(())
}

#[tokio::test]
async fn keyed_reload_retires_old_observations_before_publishing_replacements() -> Result<(), ServiceError> {
    for local in [true, false] {
        // Given
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let observer = facet("observer", move |env| { let sent = sent.clone(); env.observe(&service("keyed", local), Arc::new(move |value, context| { let result = sent.send((value, context)).map_err(|_| failure()); Box::pin(async move { result }) })) });
        let make = |value| facet("provider", move |env| { let spawner = env.provide_many(&service("keyed", local))?; let value = implementation(value, local)?; env.on_activate(Box::new(move || { let result = spawner.spawn(&"current".into(), value); Box::pin(async move { let _release = result?; Ok(()) }) })) });
        let host = create_facet_host(options(vec![observer, make("A")])).await?;
        let (old, old_context) = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
        let old = old.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        // When
        host.reload(vec![make("B")]).await?;
        let (new, new_context) = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
        let new = new.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        // Then
        assert!(old_context.abort_signal().is_some_and(|s| s.aborted()));
        assert!(old.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
        assert_eq!(new.member(&"read".into()).invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("B".into())));
        assert_eq!(host.services().catalogue().is_empty(), local);
        host.dispose().await?; assert!(new_context.abort_signal().is_some_and(|s| s.aborted()));
    } Ok(())
}

#[tokio::test]
async fn remote_state_member_survives_provider_reload_and_unsubscribes_on_disposal() -> Result<(), ServiceError> {
    // Given
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let observer = facet("observer", move |env| {
        let handle = env.use_service(&service("watched", false))?.downcast::<FacetServiceHandle>().map_err(|_| failure())?;
        let owned = env.clone_environment(); let sent = sent.clone();
        env.on_activate(Box::new(move || Box::pin(async move {
            let subscription = handle.member(&"state".into()).subscribe(Box::new(move |value, _, _| { let result = sent.send((*value).clone()).map_err(|_| failure()); Box::pin(async move { result }) }))?;
            owned.own(subscription)
        })))
    });
    let sources = Arc::new(Mutex::new(Vec::new()));
    let make = |value: &'static str| { let sources = sources.clone(); facet("provider", move |env| {
        let state = replicated_state(JsonValue::object(vec![("value".into(), JsonValue::String(value.into()))]))?;
        lock(&sources).push(state.clone());
        let implementation = maho_chord::services::provider::ProviderImplementation::new(Arc::new(()), std::collections::BTreeMap::from([("state".into(), maho_chord::services::provider::ProviderMember::State(state))]))?;
        env.provide(&service("watched", false), implementation)
    }) };
    let host = create_facet_host(options(vec![observer, make("A")])).await?;
    let initial = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // When
    host.reload(vec![make("B")]).await?;
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_ne!(initial, replacement);
    let old = lock(&sources).first().cloned().ok_or_else(failure)?;
    old.replace(BACKGROUND_CONTEXT.clone(), JsonValue::object(vec![("value".into(), JsonValue::String("C".into()))]))?;
    assert!(received.try_recv().is_err()); host.dispose().await?;
    let new = lock(&sources).last().cloned().ok_or_else(failure)?;
    new.replace(BACKGROUND_CONTEXT.clone(), JsonValue::object(vec![("value".into(), JsonValue::String("D".into()))]))?;
    assert!(received.try_recv().is_err()); Ok(())
}
