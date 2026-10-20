use crate::types::*;
use super::{errors::contract, state::lock};
use std::{collections::BTreeMap, sync::{Arc, Mutex}};

pub type AccessGuard = Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync>;
pub type RemoteMethod = Arc<dyn Fn(ServiceImplementation, Vec<JsonValue>, ContextRef) -> BoxFuture<'static, Result<Option<JsonValue>, ServiceError>> + Send + Sync>;
#[derive(Clone)]
pub enum ServiceValue {
    Method(RemoteMethod),
    State(Arc<dyn ReplicatedState<JsonValue>>),
    Object(Arc<ServiceObject>),
    Json(JsonValue),
}
pub struct ServiceObject {
    pub receiver: ServiceImplementation,
    pub members: BTreeMap<JsString, ServiceValue>,
}
pub struct ServiceSlot {
    pub service_id: JsString,
    wrap_objects: bool,
    implementation: Mutex<Option<Arc<ServiceObject>>>,
}
impl ServiceSlot {
    pub fn new(service_id: &JsString, wrap_objects: bool) -> Arc<Self> { Arc::new(Self { service_id: service_id.clone(), wrap_objects, implementation: Mutex::new(None) }) }
    pub fn bind(&self, implementation: Arc<ServiceObject>) { *lock(&self.implementation) = Some(implementation); }
    pub fn unbind(&self) { *lock(&self.implementation) = None; }
    pub fn view(self: &Arc<Self>, assert_access: AccessGuard) -> Arc<ServiceView> { Arc::new(ServiceView { slot: self.clone(), assert_access, members: Mutex::new(BTreeMap::new()) }) }
    fn resolve(&self, path: &[JsString], guard: &AccessGuard) -> Result<(ServiceValue, ServiceImplementation), ServiceError> {
        guard()?;
        let mut object = lock(&self.implementation).clone().ok_or_else(|| contract(format!("Service {:?} is disconnected", self.service_id)))?;
        for (index, name) in path.iter().enumerate() {
            let value = object.members.get(name).cloned().ok_or_else(|| contract("Service member not found"))?;
            if index + 1 == path.len() { return Ok((value, object.receiver.clone())); }
            match value { ServiceValue::Object(child) => object = child, ServiceValue::Method(_) | ServiceValue::State(_) | ServiceValue::Json(_) => return Err(contract("Service member does not have properties")) }
        }
        Err(contract("Empty service member path"))
    }
}
pub struct ServiceView { slot: Arc<ServiceSlot>, assert_access: AccessGuard, members: Mutex<BTreeMap<JsString, Arc<ValueView>>> }
impl ServiceView {
    pub fn member(&self, property: &JsString) -> Result<Arc<ValueView>, ServiceError> {
        let callable = matches!(self.slot.resolve(&[property.clone()], &self.assert_access)?.0, ServiceValue::Method(_));
        Ok(lock(&self.members).entry(property.clone()).or_insert_with(|| Arc::new(ValueView { slot: self.slot.clone(), guard: self.assert_access.clone(), path: vec![property.clone()], callable, children: Mutex::new(BTreeMap::new()) })).clone())
    }
}
pub struct ValueView { slot: Arc<ServiceSlot>, guard: AccessGuard, path: Vec<JsString>, callable: bool, children: Mutex<BTreeMap<JsString, Arc<ValueView>>> }
impl ValueView {
    pub fn get(&self) -> Result<ServiceValue, ServiceError> { Ok(self.slot.resolve(&self.path, &self.guard)?.0) }
    pub fn child(&self, property: &JsString) -> Result<Arc<Self>, ServiceError> {
        if !self.slot.wrap_objects && !matches!(self.get()?, ServiceValue::Method(_)) { return Err(contract("Object wrapping is disabled")); }
        let mut path = self.path.clone(); path.push(property.clone());
        let callable = matches!(self.slot.resolve(&path, &self.guard)?.0, ServiceValue::Method(_));
        Ok(lock(&self.children).entry(property.clone()).or_insert_with(|| Arc::new(Self { slot: self.slot.clone(), guard: self.guard.clone(), path, callable, children: Mutex::new(BTreeMap::new()) })).clone())
    }
    pub async fn invoke(&self, args: Vec<JsonValue>, context: ContextRef) -> Result<Option<JsonValue>, ServiceError> {
        let (value, receiver) = self.slot.resolve(&self.path, &self.guard)?;
        if !self.callable { return Err(contract("Service member view is not callable")); }
        match value { ServiceValue::Method(method) => method(receiver, args, context).await, ServiceValue::State(_) | ServiceValue::Object(_) | ServiceValue::Json(_) => Err(contract("Service member is not callable")) }
    }
}
