use crate::facets_support;
use facets_support::*;
use maho_chord::{*, context::BACKGROUND_CONTEXT, facets::host::FacetServiceHandle, services::consumer::ServiceFacade, types::{BoxFuture, ServiceError}};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn dropped_startup_rolls_back_owned_resources() -> Result<(), ServiceError> {
    // Given
    let (entered, mut entering) = tokio::sync::mpsc::unbounded_channel();
    let (settled, mut settling) = tokio::sync::mpsc::unbounded_channel();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let log = trace.clone();
    let owner = facet("owner", move |env| {
        env.own(effect(log.clone(), "cleanup", false))?;
        let entered = entered.clone();
        env.on_activate(Box::new(move || Box::pin(async move {
            entered.send(()).map_err(|_| failure())?;
            futures::future::pending::<Result<(), ServiceError>>().await
        })))
    });
    let mut configuration = options(vec![owner]);
    configuration.on_error = Some(Arc::new(move |_| { assert!(settled.send(()).is_ok()); }));
    let startup = tokio::spawn(create_facet_host(configuration));
    tokio::time::timeout(std::time::Duration::from_secs(2), entering.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // When
    startup.abort();
    assert!(startup.await.err().ok_or_else(failure)?.is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), settling.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_eq!(*lock(&trace), ["cleanup"]);
    Ok(())
}

#[tokio::test]
async fn dropped_reload_rolls_back_before_cutover_and_allows_another_reload() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (settled, mut settling) = tokio::sync::mpsc::unbounded_channel();
    let mut configuration = options(vec![provider("provider", service("value", false), "A", trace.clone(), false, false)]);
    configuration.on_error = Some(Arc::new(move |_| { assert!(settled.send(()).is_ok()); }));
    let host = create_facet_host(configuration).await?;
    let (entered, mut entering) = tokio::sync::mpsc::unbounded_channel();
    let log = trace.clone();
    let candidate = facet("provider", move |env| {
        env.provide(&service("value", false), implementation("B", false)?)?;
        env.on_deactivate(effect(log.clone(), "dispose B", false))?;
        let entered = entered.clone();
        env.on_activate(Box::new(move || Box::pin(async move {
            entered.send(()).map_err(|_| failure())?;
            futures::future::pending::<Result<(), ServiceError>>().await
        })))
    });
    let owner = host.clone();
    let reload = tokio::spawn(async move { owner.reload(vec![candidate]).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entering.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // When
    reload.abort();
    assert!(reload.await.err().ok_or_else(failure)?.is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), settling.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_eq!(*lock(&trace), ["activate A", "dispose B"]);
    host.reload(vec![provider("provider", service("value", false), "C", trace.clone(), false, false)]).await?;
    assert_eq!(*lock(&trace), ["activate A", "dispose B", "activate C", "dispose A"]);
    host.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn dropped_disposal_finishes_cleanup_and_reports_failure_once() -> Result<(), ServiceError> {
    // Given
    let (entered, mut entering) = tokio::sync::mpsc::unbounded_channel();
    let (resume, gate) = tokio::sync::oneshot::channel();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let (settled, mut settling) = tokio::sync::mpsc::unbounded_channel();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let log = trace.clone();
    let owner = facet("owner", move |env| {
        env.own(effect(log.clone(), "last", false))?;
        let gate = lock(&gate).take().ok_or_else(failure)?;
        let entered = entered.clone();
        env.own(Box::new(move || Box::pin(async move {
            entered.send(()).map_err(|_| failure())?;
            gate.await.map_err(|_| failure())?;
            Err(failure())
        })))
    });
    let mut configuration = options(vec![owner]);
    configuration.on_error = Some(Arc::new(move |_| { assert!(settled.send(()).is_ok()); }));
    let host = create_facet_host(configuration).await?;
    let owner = host.clone();
    let disposal = tokio::spawn(async move { owner.dispose().await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entering.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // When
    disposal.abort();
    assert!(disposal.await.err().ok_or_else(failure)?.is_cancelled());
    resume.send(()).map_err(|_| failure())?;
    tokio::time::timeout(std::time::Duration::from_secs(2), settling.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_eq!(*lock(&trace), ["last"]);
    host.dispose().await?;
    assert!(host.reload(vec![]).await.is_err());
    assert!(settling.try_recv().is_err());
    Ok(())
}

struct Loader { id: &'static str, trace: Arc<Mutex<Vec<String>>>, fail_load: bool, fail_dispose: bool }

#[tokio::test]
async fn dropped_reload_after_cutover_finishes_retirement_and_abort() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (entered, mut entering) = tokio::sync::mpsc::unbounded_channel();
    let (resume, gate) = tokio::sync::oneshot::channel();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let previous = facet("provider", move |env| {
        env.provide(&service("value", false), implementation("A", false)?)?;
        let entered = entered.clone(); let gate = lock(&gate).take().ok_or_else(failure)?;
        env.on_deactivate(Box::new(move || Box::pin(async move {
            entered.send(()).map_err(|_| failure())?;
            gate.await.map_err(|_| failure())?;
            Err(failure())
        })))
    });
    let (settled, mut settling) = tokio::sync::mpsc::unbounded_channel();
    let mut configuration = options(vec![previous]);
    configuration.on_error = Some(Arc::new(move |_| { assert!(settled.send(()).is_ok()); }));
    let host = create_facet_host(configuration).await?;
    let next = provider("provider", service("value", false), "B", trace.clone(), false, false);
    let owner = host.clone(); let reload = tokio::spawn(async move { owner.reload(vec![next]).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entering.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // When
    reload.abort();
    assert!(reload.await.err().ok_or_else(failure)?.is_cancelled());
    resume.send(()).map_err(|_| failure())?;
    tokio::time::timeout(std::time::Duration::from_secs(2), settling.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    // Then
    assert_eq!(*lock(&trace), ["activate B", "dispose B"]);
    assert!(host.reload(vec![]).await.is_err());
    host.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn reload_rejects_changed_service_locality() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let host = create_facet_host(options(vec![provider("provider", service("value", false), "A", trace.clone(), false, false)])).await?;
    // When
    let result = host.reload(vec![provider("provider", service("value", true), "B", trace.clone(), false, false)]).await;
    // Then
    assert!(result.is_err());
    assert_eq!(*lock(&trace), ["activate A", "dispose B"]);
    host.dispose().await?;
    Ok(())
}
struct Loaded { facets: Vec<Arc<dyn Facet>>, id: &'static str, trace: Arc<Mutex<Vec<String>>>, fail: bool }
impl FacetLoader for Loader {
    fn load(&self) -> BoxFuture<'_, Result<Box<dyn LoadedFacets>, ServiceError>> {
        Box::pin(async { lock(&self.trace).push(format!("load {}", self.id)); if self.fail_load { return Err(failure()); }
            Ok(Box::new(Loaded { facets: vec![facet(self.id, |_| Ok(()))], id: self.id, trace: self.trace.clone(), fail: self.fail_dispose }) as Box<dyn LoadedFacets>) })
    }
}
impl LoadedFacets for Loaded {
    fn facets(&self) -> &[Arc<dyn Facet>] { &self.facets }
    fn dispose(&mut self) -> BoxFuture<'_, Result<(), ServiceError>> { lock(&self.trace).push(format!("dispose {}", self.id)); let fail = self.fail; Box::pin(async move { if fail { Err(failure()) } else { Ok(()) } }) }
}
#[tokio::test]
async fn combines_in_order_and_disposes_once_in_reverse() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let loaders = ["first", "second"].into_iter().map(|id| Arc::new(Loader { id, trace: trace.clone(), fail_load: false, fail_dispose: false }) as Arc<dyn FacetLoader>).collect();
    let loader = combine_facet_loaders(loaders);
    // When
    let mut loaded = loader.load().await?; let ids = loaded.facets().iter().map(|f| f.id().to_owned()).collect::<Vec<_>>(); loaded.dispose().await?; loaded.dispose().await?;
    // Then
    assert_eq!(ids, ["first", "second"]); assert_eq!(*lock(&trace), ["load first", "load second", "dispose second", "dispose first"]); Ok(())
}
#[tokio::test]
async fn load_failure_collects_every_cleanup_failure() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let loaders = ["first", "second", "broken"].into_iter().map(|id| Arc::new(Loader { id, trace: trace.clone(), fail_load: id == "broken", fail_dispose: true }) as Arc<dyn FacetLoader>).collect();
    // When
    let error = combine_facet_loaders(loaders).load().await.err().ok_or_else(failure)?;
    // Then
    assert_eq!(error.downcast_ref::<maho_chord::services::errors::CollectedErrors>().ok_or_else(failure)?.errors.len(), 3);
    assert_eq!(*lock(&trace), ["load first", "load second", "load broken", "dispose second", "dispose first"]); Ok(())
}
#[tokio::test]
async fn disposal_starts_all_entries_before_waiting() -> Result<(), ServiceError> {
    // Given
    struct WaitingLoaded { facets: Vec<Arc<dyn Facet>>, barrier: Arc<tokio::sync::Barrier> }
    impl LoadedFacets for WaitingLoaded {
        fn facets(&self) -> &[Arc<dyn Facet>] { &self.facets }
        fn dispose(&mut self) -> BoxFuture<'_, Result<(), ServiceError>> { Box::pin(async { self.barrier.wait().await; Err(failure()) }) }
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut loaded: Vec<Box<dyn LoadedFacets>> = (0..2).map(|_| Box::new(WaitingLoaded { facets: vec![], barrier: barrier.clone() }) as Box<dyn LoadedFacets>).collect();
    // When
    let errors = tokio::time::timeout(std::time::Duration::from_secs(2), maho_chord::facets::loader::dispose_loaded_facets(&mut loaded)).await.map_err(|_| failure())?;
    // Then
    assert_eq!(errors.len(), 2); Ok(())
}
#[tokio::test]
async fn static_loader_is_reusable() -> Result<(), ServiceError> {
    // Given
    let original = facet("static", |_| Ok(())); let loader = create_static_facet_loader(vec![original.clone()]);
    // When
    let mut first = loader.load().await?; let mut second = loader.load().await?;
    // Then
    assert!(Arc::ptr_eq(&first.facets()[0], &original)); assert!(Arc::ptr_eq(&second.facets()[0], &original)); first.dispose().await?; second.dispose().await?; Ok(())
}
#[tokio::test]
async fn cached_local_and_remote_members_switch_only_after_activation() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let retained = Arc::new(Mutex::new(Vec::<Arc<FacetServiceHandle>>::new())); let handles = retained.clone();
    let consumer = facet("consumer", move |env| { for local in [true, false] { lock(&handles).push(env.use_service(&service(if local { "local" } else { "remote" }, local))?.downcast().map_err(|_| failure())?); } Ok(()) });
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel(); let (resume, gate) = tokio::sync::oneshot::channel(); let gate = Arc::new(Mutex::new(Some(gate)));
    let make = |value: &'static str, block: bool| { let trace = trace.clone(); let gate = gate.clone(); let started = started.clone(); facet("provider", move |env| {
        for local in [true, false] { env.provide(&service(if local { "local" } else { "remote" }, local), implementation(value, local)?)?; }
        env.on_deactivate(effect(trace.clone(), value, false))?;
        if block { let gate = lock(&gate).take().ok_or_else(failure)?; let started = started.clone(); env.on_activate(Box::new(move || Box::pin(async move { started.send(()).map_err(|_| failure())?; gate.await.map_err(|_| failure()) })))?; } Ok(())
    }) };
    let host = create_facet_host(options(vec![consumer, make("A", false)])).await?;
    let handles = lock(&retained).clone(); let members = handles.iter().map(|h| h.member(&"read".into())).collect::<Vec<_>>();
    let remote = create_remote_service_binding(RemoteServiceBindingOptions { services: vec!["remote".into()], transport: host.services(), bound: true, on_error: None, assert_access: None })?;
    let facade = remote.use_service(&"remote".into())?.downcast::<ServiceFacade>().map_err(|_| failure())?; let rpc_read = facade.member(&"read".into())?; remote.ready(BACKGROUND_CONTEXT.clone()).await?;
    // When
    let next = make("B", true); let reloading = host.clone(); let reload = tokio::spawn(async move { reloading.reload(vec![next]).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), observed.recv()).await.map_err(|_| failure())?.ok_or_else(failure)?;
    for member in &members { assert_eq!(member.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("A".into()))); }
    assert_eq!(rpc_read.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("A".into())));
    resume.send(()).map_err(|_| failure())?; reload.await.map_err(|_| failure())??;
    // Then
    for member in &members { assert_eq!(member.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("B".into()))); }
    assert_eq!(rpc_read.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("B".into())));
    assert_eq!(*lock(&trace), ["A"]); remote.dispose(BACKGROUND_CONTEXT.clone()).await?; host.dispose().await?; Ok(())
}
#[tokio::test]
async fn failed_candidate_activation_keeps_old_generation() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let descriptor = service("value", false);
    let host = create_facet_host(options(vec![provider("provider", descriptor.clone(), "A", trace.clone(), false, false)])).await?;
    // When
    let result = host.reload(vec![provider("provider", descriptor, "B", trace.clone(), true, false)]).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["activate A", "activate B", "dispose B"]);
    let remote = create_remote_service_binding(RemoteServiceBindingOptions { services: vec!["value".into()], transport: host.services(), bound: true, on_error: None, assert_access: None })?;
    let facade = remote.use_service(&"value".into())?.downcast::<ServiceFacade>().map_err(|_| failure())?; remote.ready(BACKGROUND_CONTEXT.clone()).await?;
    assert_eq!(facade.member(&"read".into())?.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(JsonValue::String("A".into())));
    remote.dispose(BACKGROUND_CONTEXT.clone()).await?; host.dispose().await?; Ok(())
}
#[tokio::test]
async fn retirement_failure_terminates_after_cutover() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let descriptor = service("value", false);
    let host = create_facet_host(options(vec![provider("provider", descriptor.clone(), "A", trace.clone(), false, true)])).await?;
    // When
    let result = host.reload(vec![provider("provider", descriptor, "B", trace.clone(), false, false)]).await;
    // Then
    assert!(result.is_err()); assert!(host.reload(vec![]).await.is_err()); host.dispose().await?;
    assert_eq!(*lock(&trace), ["activate A", "activate B", "dispose A", "dispose B"]); Ok(())
}

#[tokio::test]
async fn changed_facet_shape_is_rejected_before_activation() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let host = create_facet_host(options(vec![provider("provider", service("value", false), "A", trace.clone(), false, false)])).await?;
    let candidate = provider("provider", service("different", false), "B", trace.clone(), false, false);
    // When
    let result = host.reload(vec![candidate]).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["activate A", "dispose B"]); host.reload(vec![]).await?; host.dispose().await?; Ok(())
}

#[tokio::test]
async fn changed_remote_member_shape_is_rejected_before_cutover() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let host = create_facet_host(options(vec![provider("provider", service("value", false), "A", trace.clone(), false, false)])).await?;
    let candidate = facet("provider", |env| {
        let method: maho_chord::services::handle::RemoteMethod = Arc::new(|_, _, _| Box::pin(async { Ok(None) }));
        let implementation = maho_chord::services::provider::ProviderImplementation::new(Arc::new(()), std::collections::BTreeMap::from([("renamed".into(), maho_chord::services::provider::ProviderMember::Method(method))]))?;
        env.provide(&service("value", false), implementation)
    });
    // When
    let result = host.reload(vec![candidate]).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["activate A"]); host.dispose().await?; Ok(())
}

#[tokio::test]
async fn failed_candidate_cleanup_aborts_old_generation() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let descriptor = service("value", false);
    let host = create_facet_host(options(vec![provider("provider", descriptor.clone(), "A", trace.clone(), false, false)])).await?;
    // When
    let result = host.reload(vec![provider("provider", descriptor, "B", trace.clone(), true, true)]).await;
    // Then
    assert!(result.is_err()); assert!(host.reload(vec![]).await.is_err());
    assert_eq!(*lock(&trace), ["activate A", "activate B", "dispose B", "dispose A"]); host.dispose().await?; Ok(())
}

#[tokio::test]
async fn publication_failure_terminates_host_after_cutover() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new())); let descriptor = service("value", false);
    let host = create_facet_host(options(vec![provider("provider", descriptor.clone(), "A", trace.clone(), false, false)])).await?;
    let mut subscription = host.services().subscribe("value".into(), ServiceMode::Singleton, Arc::new(|_, _| panic!("fixture publication failure")), BACKGROUND_CONTEXT.clone()).await?;
    subscription.activate()?;
    // When
    let result = host.reload(vec![provider("provider", descriptor, "B", trace, false, false)]).await;
    // Then
    assert!(result.is_err()); assert!(host.reload(vec![]).await.is_err()); host.dispose().await?; Ok(())
}

#[tokio::test]
async fn failed_candidate_activation_cleans_in_reverse_dependency_order() -> Result<(), ServiceError> {
    // Given
    let trace = Arc::new(Mutex::new(Vec::new()));
    let consumer = |fail| { let trace = trace.clone(); facet("consumer", move |env| { env.use_service(&service("value", false))?; env.on_activate(effect(trace.clone(), "activate consumer", fail))?; env.on_deactivate(effect(trace.clone(), "dispose consumer", false)) }) };
    let host = create_facet_host(options(vec![consumer(false), provider("provider", service("value", false), "A", trace.clone(), false, false)])).await?;
    lock(&trace).clear();
    // When
    let result = host.reload(vec![consumer(true), provider("provider", service("value", false), "B", trace.clone(), false, false)]).await;
    // Then
    assert!(result.is_err()); assert_eq!(*lock(&trace), ["activate B", "activate consumer", "dispose consumer", "dispose B"]); host.dispose().await?; Ok(())
}
