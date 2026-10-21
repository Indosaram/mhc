use crate::{facets::{host::FacetKernel, loader::dispose_loaded_facets}, services::{consumer::RemoteServiceBindingImpl, errors::{collected, contract, CollectedErrors}, state::{attach_replicated_state_source, AttachedReplicatedStateImpl, MutableReplicatedStateImpl}}, types::*};
use std::{marker::PhantomData, sync::Arc};

pub async fn create_facet_host(options: FacetOptions) -> Result<Arc<dyn FacetHost>, ServiceError> {
    let kernel = FacetKernel::new(options)?;
    kernel.activate().await?;
    let provider = kernel.provider()?;
    Ok(Arc::new(ActiveFacetHost { kernel, provider }))
}
struct ActiveFacetHost { kernel: Arc<FacetKernel>, provider: Arc<crate::services::provider::RemoteServiceProvider> }
impl FacetHost for ActiveFacetHost {
    fn services(&self) -> Arc<dyn RemoteServiceProvider> { self.provider.clone() }
    fn reload(&self, facets: Vec<Arc<dyn Facet>>) -> BoxFuture<'_, Result<(), ServiceError>> { Box::pin(self.kernel.reload(facets)) }
    fn dispose(&self) -> BoxFuture<'_, Result<(), ServiceError>> { Box::pin(self.kernel.dispose()) }
}

pub fn create_static_facet_loader(facets: Vec<Arc<dyn Facet>>) -> Arc<dyn FacetLoader> { Arc::new(StaticFacetLoader { facets }) }
struct StaticFacetLoader { facets: Vec<Arc<dyn Facet>> }
struct StaticLoadedFacets { facets: Vec<Arc<dyn Facet>> }
impl FacetLoader for StaticFacetLoader {
    fn load(&self) -> BoxFuture<'_, Result<Box<dyn LoadedFacets>, ServiceError>> { Box::pin(async { Ok(Box::new(StaticLoadedFacets { facets: self.facets.clone() }) as Box<dyn LoadedFacets>) }) }
}
impl LoadedFacets for StaticLoadedFacets {
    fn facets(&self) -> &[Arc<dyn Facet>] { &self.facets }
    fn dispose(&mut self) -> BoxFuture<'_, Result<(), ServiceError>> { Box::pin(async { Ok(()) }) }
}

pub fn combine_facet_loaders(loaders: Vec<Arc<dyn FacetLoader>>) -> Arc<dyn FacetLoader> { Arc::new(CombinedFacetLoader { loaders }) }
struct CombinedFacetLoader { loaders: Vec<Arc<dyn FacetLoader>> }
struct CombinedLoadedFacets { facets: Vec<Arc<dyn Facet>>, loaded: Vec<Box<dyn LoadedFacets>>, disposed: bool }
impl FacetLoader for CombinedFacetLoader {
    fn load(&self) -> BoxFuture<'_, Result<Box<dyn LoadedFacets>, ServiceError>> {
        Box::pin(async {
            let mut loaded = Vec::new();
            for loader in &self.loaders { match loader.load().await {
                Ok(generation) => loaded.push(generation),
                Err(error) => { loaded.reverse(); let cleanup = dispose_loaded_facets(&mut loaded).await; if cleanup.is_empty() { return Err(error); } let mut errors = vec![error]; errors.extend(cleanup); return Err(Box::new(CollectedErrors { message: "Facet loading and cleanup failed", errors }) as ServiceError); }
            } }
            let facets = loaded.iter().flat_map(|entry| entry.facets().iter().cloned()).collect();
            Ok(Box::new(CombinedLoadedFacets { facets, loaded, disposed: false }) as Box<dyn LoadedFacets>)
        })
    }
}
impl LoadedFacets for CombinedLoadedFacets {
    fn facets(&self) -> &[Arc<dyn Facet>] { &self.facets }
    fn dispose(&mut self) -> BoxFuture<'_, Result<(), ServiceError>> {
        Box::pin(async { if self.disposed { return Ok(()); } self.disposed = true; self.loaded.reverse(); collected(dispose_loaded_facets(&mut self.loaded).await, "Failed to dispose loaded facets") })
    }
}

pub fn define_facet(facet: Arc<dyn Facet>) -> Arc<dyn Facet> { facet }

pub fn define_service<T>(id: impl Into<JsString>, local: bool) -> Result<Service<T>, ServiceError> {
    let id = id.into();
    if id.is_empty() { return Err(contract("Service ID must not be empty")); }
    if id.units().starts_with(&"$chord.".encode_utf16().collect::<Vec<_>>()) { return Err(contract("Service IDs beginning with $chord. are reserved")); }
    Ok(Service { id, local, service_type: PhantomData })
}

pub fn create_remote_service_binding(options: RemoteServiceBindingOptions) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> { RemoteServiceBindingImpl::new(options) }

pub trait ReplicatedStateInput {
    type State;
    fn attach(self) -> Result<Self::State, ServiceError>;
}
impl ReplicatedStateInput for JsonValue {
    type State = Arc<MutableReplicatedStateImpl>;
    fn attach(self) -> Result<Self::State, ServiceError> { MutableReplicatedStateImpl::new(self) }
}
impl<T: Send + Sync + 'static> ReplicatedStateInput for (&dyn ReplicatedStateSource<T>, ReplicatedStateSourceOptions) {
    type State = Arc<AttachedReplicatedStateImpl<T>>;
    fn attach(self) -> Result<Self::State, ServiceError> { attach_replicated_state_source(self.0, self.1) }
}
impl<T: Send + Sync + 'static> ReplicatedStateInput for &dyn ReplicatedStateSource<T> {
    type State = Arc<AttachedReplicatedStateImpl<T>>;
    fn attach(self) -> Result<Self::State, ServiceError> { attach_replicated_state_source(self, ReplicatedStateSourceOptions { on_error: None }) }
}
pub fn replicated_state<I: ReplicatedStateInput>(input: I) -> Result<I::State, ServiceError> { input.attach() }

pub fn replicated_state_from_source<T: Send + Sync + 'static>(source: &dyn ReplicatedStateSource<T>, options: ReplicatedStateSourceOptions) -> Result<Arc<AttachedReplicatedStateImpl<T>>, ServiceError> { attach_replicated_state_source(source, options) }
