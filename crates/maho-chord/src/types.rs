//! Port of `chord/src/types.ts`. Rust traits replace structural TS contracts.
use crate::delta::{tracker::Change, Op};
pub use crate::js_string::JsString;
use serde::{Deserialize, Serialize};
use std::{any::Any, future::Future, marker::PhantomData, pin::Pin, sync::Arc};

pub type JsonObject = Vec<(JsString, JsonValue)>;

/// A finite nonnegative integral JavaScript Number, including values beyond u64.
#[derive(Clone, Copy, Debug)]
pub struct ServiceSequence(f64);
impl ServiceSequence {
    pub const ZERO: Self = Self(0.0);
    pub fn new(value: f64) -> Result<Self, ServiceError> {
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
            return Err("Service sequence must be a nonnegative integral JS Number".into());
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }
    pub fn number(self) -> f64 { self.0 }
    pub fn next(self) -> Self { Self(self.0 + 1.0) }
}
impl PartialEq for ServiceSequence {
    fn eq(&self, other: &Self) -> bool { self.0.to_bits() == other.0.to_bits() }
}
impl Eq for ServiceSequence {}
impl PartialOrd for ServiceSequence {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl Ord for ServiceSequence {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.total_cmp(&other.0) }
}
impl std::hash::Hash for ServiceSequence {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) { std::hash::Hash::hash(&self.0.to_bits(), state); }
}
impl Serialize for ServiceSequence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Sequences are nonnegative integers; the rounded f64 bound is 2^64.
        if self.0 < u64::MAX as f64 {
            serializer.serialize_u64(self.0 as u64)
        } else {
            serializer.serialize_f64(self.0)
        }
    }
}
impl<'de> Deserialize<'de> for ServiceSequence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Immutable JSON with cheap revision ownership and shallow copy-on-write.
/// Objects retain insertion order, including own reserved-name data properties.
#[derive(Clone, Debug)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(JsString),
    Array(Arc<Vec<JsonValue>>),
    Object(Arc<JsonObject>),
}

mod object_serde {
    use super::*;
    use serde::ser::SerializeMap;
    pub fn serialize<S: serde::Serializer>(
        value: &Arc<JsonObject>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if value.iter().any(|(key, _)| key.as_str().is_none()) {
            use std::fmt::Write;
            let mut json = String::from("{");
            for (index, (key, value)) in value.iter().enumerate() {
                if index != 0 { json.push(','); }
                write!(json, "{}:{}", serde_json::to_string(key).map_err(serde::ser::Error::custom)?, serde_json::to_string(value).map_err(serde::ser::Error::custom)?)
                    .map_err(serde::ser::Error::custom)?;
            }
            json.push('}');
            return serde_json::value::RawValue::from_string(json)
                .map_err(serde::ser::Error::custom)?.serialize(serializer);
        }
        let mut map = serializer.serialize_map(Some(value.len()))?;
        for (key, value) in value.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Serialize for JsonValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => value.serialize(serializer),
            Self::Number(value) => value.serialize(serializer),
            Self::String(value) => value.serialize(serializer),
            Self::Array(value) => value.serialize(serializer),
            Self::Object(value) => object_serde::serialize(value, serializer),
        }
    }
}
impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct("$serde_json::private::RawValue", RawJsonVisitor)
    }
}

struct JsonVisitor;
struct RawJsonVisitor;
impl<'de> serde::de::Visitor<'de> for RawJsonVisitor {
    type Value = JsonValue;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("raw JSON value")
    }
    fn visit_newtype_struct<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> Result<JsonValue, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<JsonValue, A::Error> {
        if map.next_key::<String>()?.as_deref() != Some("$serde_json::private::RawValue") {
            return Err(serde::de::Error::custom("invalid raw JSON"));
        }
        crate::json::parse(&map.next_value::<String>()?).map_err(serde::de::Error::custom)
    }
}
impl<'de> serde::de::Visitor<'de> for JsonVisitor {
    type Value = JsonValue;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON value")
    }
    fn visit_newtype_struct<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> Result<JsonValue, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<JsonValue, E> {
        Ok(JsonValue::Null)
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<JsonValue, E> {
        Ok(JsonValue::Bool(value))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<JsonValue, E> {
        Ok(JsonValue::Number(value.into()))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<JsonValue, E> {
        Ok(JsonValue::Number(value.into()))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<JsonValue, E> {
        serde_json::Number::from_f64(value)
            .map(JsonValue::Number)
            .ok_or_else(|| E::custom("nonfinite number"))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<JsonValue, E> {
        Ok(JsonValue::String(value.into()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<JsonValue, E> {
        Ok(JsonValue::String(value.into()))
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut sequence: A,
    ) -> Result<JsonValue, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(JsonValue::array(values))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<JsonValue, A::Error> {
        let mut values = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            values.push((key.into(), map.next_value()?));
        }
        Ok(JsonValue::object(values))
    }
}

impl PartialEq for JsonValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b || a.as_f64() == b.as_f64(),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => Arc::ptr_eq(a, b) || a == b,
            (Self::Object(a), Self::Object(b)) => {
                Arc::ptr_eq(a, b)
                    || (a.len() == b.len()
                        && a.iter()
                            .all(|(k, v)| b.iter().any(|(ok, ov)| k == ok && v == ov)))
            }
            _ => false,
        }
    }
}
impl JsonValue {
    pub fn array(values: Vec<Self>) -> Self {
        Self::Array(Arc::new(values))
    }
    pub fn object(values: JsonObject) -> Self {
        let mut result: JsonObject = Vec::with_capacity(values.len());
        for (key, value) in values {
            if let Some((_, existing)) = result.iter_mut().find(|(k, _)| k == &key) {
                *existing = value;
            } else {
                result.push((key, value));
            }
        }
        result.sort_by(|(a, _), (b, _)| {
            let index = |key: &JsString| key.array_index();
            match (index(a), index(b)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
        Self::Object(Arc::new(result))
    }
    pub fn is_container(&self) -> bool {
        matches!(self, Self::Array(_) | Self::Object(_))
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Array(a), Self::Array(b)) => Arc::ptr_eq(a, b),
            (Self::Object(a), Self::Object(b)) => Arc::ptr_eq(a, b),
            _ => !self.is_container() && self == other,
        }
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ServiceError = Box<dyn std::error::Error + Send + Sync>;
pub type Disposal = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), ServiceError>> + Send>;

/// Identity is the allocation, never its debug description or value type.
pub struct ContextKey<T> {
    pub token: Arc<str>,
    pub identity: Arc<()>,
    pub value_type: PhantomData<fn(T) -> T>,
}
pub trait Context: Send + Sync {
    fn abort_signal(&self) -> Option<Arc<dyn AbortSignal>>;
    fn value(&self, identity: &Arc<()>) -> Option<Arc<dyn Any + Send + Sync>>;
    fn to_string(&self) -> String;
}
pub trait AbortSignal: Send + Sync {
    fn aborted(&self) -> bool;
    fn cancellation_order(&self) -> Option<u64> {
        None
    }
    fn reason(&self) -> Option<Arc<dyn std::error::Error + Send + Sync>> {
        None
    }
    fn cancelled(&self) -> BoxFuture<'_, ()>;
}
pub type ContextRef = Arc<dyn Context>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicatedStateDeliveryKind {
    Hydrate,
    Update,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplicatedStateDelivery {
    pub kind: ReplicatedStateDeliveryKind,
    pub sequence: ServiceSequence,
}
pub type StateListener<T> = Box<
    dyn Fn(
            Arc<T>,
            ContextRef,
            ReplicatedStateDelivery,
        ) -> BoxFuture<'static, Result<(), ServiceError>>
        + Send
        + Sync,
>;
pub trait ReplicatedState<T>: Send + Sync {
    fn value(&self) -> Option<Arc<T>>;
    fn subscribe(&self, listener: StateListener<T>) -> Disposal;
}
pub trait MutableReplicatedState: ReplicatedState<JsonValue> {
    fn change(
        &self,
        context: ContextRef,
        mutate: &mut dyn FnMut(&mut Change) -> Result<(), ServiceError>,
    ) -> Result<(), ServiceError>;
    fn replace(&self, context: ContextRef, value: JsonValue) -> Result<(), ServiceError>;
}
pub struct ReplicatedStateSourceFrame<T> {
    pub cursor: i64,
    pub value: Arc<T>,
    pub ops: Arc<[Op]>,
    pub context: ContextRef,
}
pub struct ReplicatedStateSourceSnapshot<T> {
    pub value: Arc<T>,
    pub cursor: i64,
}
pub trait ReplicatedStateSourceAttachment<T>: Send {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<T>;
    fn activate(
        &mut self,
        listener: Box<dyn FnMut(ReplicatedStateSourceFrame<T>) + Send>,
    ) -> Result<(), ServiceError>;
    fn dispose(&mut self);
}
pub trait ReplicatedStateSource<T>: Send + Sync {
    fn attach(&self) -> Result<Box<dyn ReplicatedStateSourceAttachment<T>>, ServiceError>;
}
pub type ErrorHandler = Arc<dyn Fn(ServiceError) + Send + Sync>;
pub struct ReplicatedStateSourceOptions {
    pub on_error: Option<ErrorHandler>,
}
pub trait AttachedReplicatedState<T>: ReplicatedState<T> {
    fn dispose(&self);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    Singleton,
    Keyed,
}
pub struct Service<T> {
    pub id: JsString,
    pub local: bool,
    pub service_type: PhantomData<fn(T) -> T>,
}
pub trait ServiceSpawner<T>: Send + Sync {
    fn spawn(&self, key: &JsString, implementation: T) -> Result<Disposal, ServiceError>;
}
pub type ServiceImplementation = Arc<dyn Any + Send + Sync>;
pub type ServiceObserver = Arc<
    dyn Fn(ServiceImplementation, ContextRef) -> BoxFuture<'static, Result<(), ServiceError>>
        + Send
        + Sync,
>;
pub trait RemoteServices: Send + Sync {
    fn use_service(&self, id: &JsString) -> Result<ServiceImplementation, ServiceError>;
    fn observe(&self, id: &JsString, handler: ServiceObserver) -> Result<Disposal, ServiceError>;
    fn ready(&self, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>>;
    fn dispose(&self, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCatalogueEntry {
    pub service_id: JsString,
    pub mode: ServiceMode,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInstanceAddress {
    pub key: JsString,
    pub generation: ServiceSequence,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ServiceMemberSnapshot {
    Method {
        name: JsString,
    },
    State {
        name: JsString,
        sequence: ServiceSequence,
        ops: Vec<Op>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInstanceSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<ServiceInstanceAddress>,
    pub members: Vec<ServiceMemberSnapshot>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSubscriptionSnapshot {
    pub service_id: JsString,
    pub mode: ServiceMode,
    pub instances: Vec<ServiceInstanceSnapshot>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServiceProviderUpdate {
    State {
        #[serde(skip_serializing_if = "Option::is_none")]
        instance: Option<ServiceInstanceAddress>,
        member: JsString,
        sequence: ServiceSequence,
        ops: Vec<Op>,
    },
    Reset {
        snapshot: ServiceSubscriptionSnapshot,
    },
    Unavailable,
    Replaced {
        snapshot: ServiceInstanceSnapshot,
    },
    Spawned {
        instance: ServiceInstanceSnapshot,
    },
    Closed {
        instance: ServiceInstanceAddress,
    },
}
crate::json::deserialize_tagged!(ServiceMemberSnapshot, "kind", {
    "method" => Method { name },
    "state" => State { name, sequence, ops },
});
crate::json::deserialize_tagged!(ServiceProviderUpdate, "type", {
    "state" => State { instance, member, sequence, ops },
    "reset" => Reset { snapshot },
    "unavailable" => Unavailable {},
    "replaced" => Replaced { snapshot },
    "spawned" => Spawned { instance },
    "closed" => Closed { instance },
});
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCall {
    pub service_id: JsString,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<ServiceInstanceAddress>,
    pub member: JsString,
    pub args: Vec<JsonValue>,
}
pub trait ServiceSubscription: Send {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot;
    fn activate(&mut self) -> Result<(), ServiceError>;
    fn close(&mut self, context: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>>;
}
pub trait RemoteServiceTransport: Send + Sync {
    fn invoke(
        &self,
        call: ServiceCall,
        context: ContextRef,
    ) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>>;
    fn subscribe(
        &self,
        service_id: JsString,
        mode: ServiceMode,
        listener: Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>,
        context: ContextRef,
    ) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>>;
}
pub struct RemoteServiceBindingOptions {
    pub services: Vec<JsString>,
    pub transport: Arc<dyn RemoteServiceTransport>,
    pub bound: bool,
    pub on_error: Option<ErrorHandler>,
    pub assert_access: Option<Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync>>,
}
pub trait RemoteServiceBinding: RemoteServices {
    fn rebind(&self, bound: bool, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDescriptor {
    pub id: JsString,
    pub local: bool,
}
impl<T> Service<T> {
    pub fn descriptor(&self) -> ServiceDescriptor {
        ServiceDescriptor {
            id: self.id.clone(),
            local: self.local,
        }
    }
}
pub trait FacetEnvironment: Send + Sync {
    fn clone_environment(&self) -> Arc<dyn FacetEnvironment>;
    fn use_service(
        &self,
        service: &ServiceDescriptor,
    ) -> Result<ServiceImplementation, ServiceError>;
    fn observe(
        &self,
        service: &ServiceDescriptor,
        handler: ServiceObserver,
    ) -> Result<(), ServiceError>;
    fn provide(
        &self,
        service: &ServiceDescriptor,
        implementation: ServiceImplementation,
    ) -> Result<(), ServiceError>;
    fn provide_many(
        &self,
        service: &ServiceDescriptor,
    ) -> Result<Box<dyn ServiceSpawner<ServiceImplementation>>, ServiceError>;
    fn replicated_state(
        &self,
        initial: JsonValue,
    ) -> Result<Arc<dyn MutableReplicatedState>, ServiceError>;
    fn own(&self, disposal: Disposal) -> Result<(), ServiceError>;
    fn on_activate(&self, callback: Disposal) -> Result<(), ServiceError>;
    fn on_deactivate(&self, callback: Disposal) -> Result<(), ServiceError>;
}
pub trait Facet: Send + Sync {
    fn id(&self) -> &str;
    fn setup(&self, env: &mut dyn FacetEnvironment) -> Result<(), ServiceError>;
}
pub trait RemoteServiceSource: Send + Sync {
    fn accepts_unavailable_services(&self) -> bool;
    fn catalogue(
        &self,
        context: ContextRef,
    ) -> BoxFuture<'_, Result<Vec<ServiceCatalogueEntry>, ServiceError>>;
    fn open(
        &self,
        services: &[JsString],
        assert_access: Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync>,
        on_error: ErrorHandler,
    ) -> Result<Arc<dyn RemoteServices>, ServiceError>;
}
pub struct FacetOptions {
    pub facets: Vec<Arc<dyn Facet>>,
    pub service_sources: Vec<Arc<dyn RemoteServiceSource>>,
    pub on_error: Option<ErrorHandler>,
}
pub trait RemoteServiceProvider: RemoteServiceTransport {
    fn catalogue(&self) -> Vec<ServiceCatalogueEntry>;
}
pub trait FacetHost: Send + Sync {
    fn services(&self) -> Arc<dyn RemoteServiceProvider>;
    fn reload(&self, facets: Vec<Arc<dyn Facet>>) -> BoxFuture<'_, Result<(), ServiceError>>;
    fn dispose(&self) -> BoxFuture<'_, Result<(), ServiceError>>;
}
pub trait LoadedFacets: Send {
    fn facets(&self) -> &[Arc<dyn Facet>];
    fn dispose(&mut self) -> BoxFuture<'_, Result<(), ServiceError>>;
}
pub trait FacetLoader: Send + Sync {
    fn load(&self) -> BoxFuture<'_, Result<Box<dyn LoadedFacets>, ServiceError>>;
}
