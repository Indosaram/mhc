use maho_chord::{services::{handle::{ServiceObject, ServiceValue}, provider::{ProviderImplementation, ProviderMember}}, types::*};
use std::{collections::BTreeMap, sync::{Arc, Mutex, MutexGuard}};
pub fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> { value.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
pub fn failure() -> ServiceError { Box::new(std::io::Error::other("fixture failure")) }
type Setup = dyn Fn(&mut dyn FacetEnvironment) -> Result<(), ServiceError> + Send + Sync;
struct TestFacet { id: String, setup: Box<Setup> }
impl Facet for TestFacet {
    fn id(&self) -> &str { &self.id }
    fn setup(&self, env: &mut dyn FacetEnvironment) -> Result<(), ServiceError> { (self.setup)(env) }
}
pub fn facet(id: &str, setup: impl Fn(&mut dyn FacetEnvironment) -> Result<(), ServiceError> + Send + Sync + 'static) -> Arc<dyn Facet> { Arc::new(TestFacet { id: id.into(), setup: Box::new(setup) }) }
pub fn options(facets: Vec<Arc<dyn Facet>>) -> FacetOptions { FacetOptions { facets, service_sources: Vec::new(), on_error: None } }
pub fn service(id: &str, local: bool) -> ServiceDescriptor { ServiceDescriptor { id: id.into(), local } }
pub fn implementation(value: &'static str, local: bool) -> Result<ServiceImplementation, ServiceError> {
    let method: maho_chord::services::handle::RemoteMethod = Arc::new(move |_, _, _| Box::pin(async move { Ok(Some(JsonValue::String(value.into()))) }));
    if local { Ok(Arc::new(ServiceObject { receiver: Arc::new(value), members: BTreeMap::from([("read".into(), ServiceValue::Method(method))]) })) }
    else { Ok(ProviderImplementation::new(Arc::new(value), BTreeMap::from([("read".into(), ProviderMember::Method(method))]))?) }
}
pub fn effect(trace: Arc<Mutex<Vec<String>>>, label: &str, fail: bool) -> Disposal {
    let label = label.to_owned(); Box::new(move || { lock(&trace).push(label); Box::pin(async move { if fail { Err(failure()) } else { Ok(()) } }) })
}
pub fn provider(id: &str, descriptor: ServiceDescriptor, value: &'static str, trace: Arc<Mutex<Vec<String>>>, fail_activation: bool, fail_cleanup: bool) -> Arc<dyn Facet> {
    facet(id, move |env| { env.provide(&descriptor, implementation(value, descriptor.local)?)?;
        env.on_activate(effect(trace.clone(), &format!("activate {value}"), fail_activation))?;
        env.on_deactivate(effect(trace.clone(), &format!("dispose {value}"), fail_cleanup)) })
}
