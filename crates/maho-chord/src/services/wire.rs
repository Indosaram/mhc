use super::errors::contract;
use crate::{
    delta::{valid_op, valid_wire_op, WireOp},
    types::*,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum WireServiceMemberSnapshot {
    Method {
        name: JsString,
    },
    State {
        name: JsString,
        sequence: ServiceSequence,
        ops: Vec<WireOp>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireServiceInstanceSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<ServiceInstanceAddress>,
    pub members: Vec<WireServiceMemberSnapshot>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireServiceSubscriptionSnapshot {
    pub service_id: JsString,
    pub mode: ServiceMode,
    pub instances: Vec<WireServiceInstanceSnapshot>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum WireServiceProviderUpdate {
    State {
        #[serde(skip_serializing_if = "Option::is_none")]
        instance: Option<ServiceInstanceAddress>,
        member: JsString,
        sequence: ServiceSequence,
        ops: Vec<WireOp>,
    },
    Reset {
        snapshot: WireServiceSubscriptionSnapshot,
    },
    Unavailable,
    Replaced {
        snapshot: WireServiceInstanceSnapshot,
    },
    Spawned {
        instance: WireServiceInstanceSnapshot,
    },
    Closed {
        instance: ServiceInstanceAddress,
    },
}
crate::json::deserialize_tagged!(WireServiceMemberSnapshot, "kind", {
    "method" => Method { name },
    "state" => State { name, sequence, ops },
});
crate::json::deserialize_tagged!(WireServiceProviderUpdate, "type", {
    "state" => State { instance, member, sequence, ops },
    "reset" => Reset { snapshot },
    "unavailable" => Unavailable {},
    "replaced" => Replaced { snapshot },
    "spawned" => Spawned { instance },
    "closed" => Closed { instance },
});
#[derive(Debug, PartialEq, Eq)]
pub enum ServiceControlCall {
    Catalogue,
    Subscribe {
        subscription_id: JsString,
        service_id: JsString,
        mode: ServiceMode,
    },
    Unsubscribe {
        subscription_id: JsString,
    },
}
pub fn create_service_catalogue_call() -> ServiceCall {
    control("catalogue", vec![])
}
pub fn create_service_subscribe_call(
    subscription_id: &JsString,
    service_id: &JsString,
    mode: ServiceMode,
) -> ServiceCall {
    control(
        "subscribe",
        vec![
            JsonValue::String(subscription_id.clone()),
            JsonValue::String(service_id.clone()),
            JsonValue::String(
                match mode {
                    ServiceMode::Singleton => "singleton",
                    ServiceMode::Keyed => "keyed",
                }
                .into(),
            ),
        ],
    )
}
pub fn create_service_unsubscribe_call(subscription_id: &JsString) -> ServiceCall {
    control(
        "unsubscribe",
        vec![JsonValue::String(subscription_id.clone())],
    )
}
fn control(member: &str, args: Vec<JsonValue>) -> ServiceCall {
    ServiceCall {
        service_id: "$chord.service".into(),
        instance: None,
        member: member.into(),
        args,
    }
}
pub fn decode_service_control_call(call: &ServiceCall) -> Option<ServiceControlCall> {
    if call.service_id != "$chord.service" || call.instance.is_some() {
        return None;
    }
    match (call.member.as_str(), call.args.as_slice()) {
        (Some("catalogue"), []) => Some(ServiceControlCall::Catalogue),
        (
            Some("subscribe"),
            [JsonValue::String(subscription), JsonValue::String(service), JsonValue::String(mode)],
        ) if !subscription.is_empty() && !service.is_empty() => {
            let mode = match mode.as_str() {
                Some("singleton") => ServiceMode::Singleton,
                Some("keyed") => ServiceMode::Keyed,
                _ => return None,
            };
            Some(ServiceControlCall::Subscribe {
                subscription_id: subscription.clone(),
                service_id: service.clone(),
                mode,
            })
        }
        (Some("unsubscribe"), [JsonValue::String(id)]) if !id.is_empty() => {
            Some(ServiceControlCall::Unsubscribe {
                subscription_id: id.clone(),
            })
        }
        _ => None,
    }
}
fn keys(value: &JsonValue, required: &[&str], optional: &[&str]) -> Result<(), ServiceError> {
    let object = value
        .as_object()
        .ok_or_else(|| contract("Invalid service object"))?;
    if required
        .iter()
        .any(|k| !object.iter().any(|(key, _)| key == *k))
        || object
            .iter()
            .map(|(key, _)| key)
            .any(|k| !k.as_str().is_some_and(|key| required.contains(&key) || optional.contains(&key)))
    {
        return Err(contract("Invalid service object fields"));
    }
    Ok(())
}
fn id(value: &JsonValue) -> Result<(), ServiceError> {
    if matches!(value, JsonValue::String(v) if !v.is_empty()) {
        Ok(())
    } else {
        Err(contract("Invalid service ID"))
    }
}
fn address(value: &JsonValue) -> Result<(), ServiceError> {
    keys(value, &["key", "generation"], &[])?;
    id(&value["key"])?;
    js_integer(&value["generation"], 1)?;
    Ok(())
}
fn js_integer(value: &JsonValue, minimum: u64) -> Result<ServiceSequence, ServiceError> {
    let number = match value {
        JsonValue::Number(number) => number.as_f64(),
        _ => None,
    }.ok_or_else(|| contract("Invalid service integer"))?;
    let sequence = ServiceSequence::new(number)?;
    if sequence.number() < minimum as f64 { return Err(contract("Invalid service integer")); }
    Ok(sequence)
}
fn sequence_json(sequence: ServiceSequence) -> Result<JsonValue, ServiceError> {
    serde_json::to_value(sequence)
        .map(JsonValue::from)
        .map_err(|_| contract("Invalid service integer"))
}
fn normalize_address(value: &JsonValue) -> Result<JsonValue, ServiceError> {
    address(value)?;
    Ok(JsonValue::object(vec![("key".into(), value["key"].clone()), ("generation".into(), sequence_json(js_integer(&value["generation"], 1)?)?)]))
}
fn normalize_instance(value: &JsonValue) -> Result<JsonValue, ServiceError> {
    let members = value["members"].as_array().ok_or_else(|| contract("Invalid service members"))?;
    let members = members.iter().map(|member| {
        let mut fields = member.as_object().ok_or_else(|| contract("Invalid service member"))?.clone();
        if member["kind"].as_str() == Some("state") { for (key, value) in &mut fields { if key == "sequence" { *value = sequence_json(js_integer(value, 0)?)?; } } }
        Ok(JsonValue::object(fields))
    }).collect::<Result<Vec<_>, ServiceError>>()?;
    let mut fields = vec![("members".into(), JsonValue::array(members))];
    if let Some(address) = value.get("instance") { fields.push(("instance".into(), normalize_address(address)?)); }
    Ok(JsonValue::object(fields))
}
fn normalize_snapshot(value: &JsonValue) -> Result<JsonValue, ServiceError> {
    let instances = value["instances"].as_array().ok_or_else(|| contract("Invalid service instances"))?.iter().map(normalize_instance).collect::<Result<Vec<_>, _>>()?;
    Ok(JsonValue::object(vec![("serviceId".into(), value["serviceId"].clone()), ("mode".into(), value["mode"].clone()), ("instances".into(), JsonValue::array(instances))]))
}
fn normalize_update(value: &JsonValue) -> Result<JsonValue, ServiceError> {
    let mut fields = value.as_object().ok_or_else(|| contract("Invalid service update"))?.clone();
    for (key, field) in &mut fields {
        match key.as_str() {
            Some("sequence") => *field = sequence_json(js_integer(field, 1)?)?,
            Some("snapshot") => *field = if value["type"].as_str() == Some("reset") { normalize_snapshot(field)? } else { normalize_instance(field)? },
            Some("instance") => *field = if value["type"].as_str() == Some("spawned") { normalize_instance(field)? } else { normalize_address(field)? },
            _ => {},
        }
    }
    Ok(JsonValue::object(fields))
}
pub fn parse_service_call(value: impl Into<JsonValue>) -> Result<ServiceCall, ServiceError> {
    let value = value.into();
    keys(&value, &["serviceId", "member", "args"], &["instance"])?;
    id(&value["serviceId"])?;
    id(&value["member"])?;
    if let Some(instance) = value.get("instance") {
        address(instance)?;
    }
    let value = if let Some(instance) = value.get("instance") { JsonValue::object(vec![("serviceId".into(), value["serviceId"].clone()), ("member".into(), value["member"].clone()), ("args".into(), value["args"].clone()), ("instance".into(), normalize_address(instance)?)]) } else { value };
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
pub fn parse_service_catalogue(
    value: impl Into<JsonValue>,
) -> Result<Vec<ServiceCatalogueEntry>, ServiceError> {
    let value = value.into();
    let entries = value
        .as_array()
        .ok_or_else(|| contract("Invalid service catalogue"))?;
    let mut ids = std::collections::HashSet::new();
    for entry in entries {
        keys(entry, &["serviceId", "mode"], &[])?;
        id(&entry["serviceId"])?;
        if !ids.insert(match &entry["serviceId"] { JsonValue::String(id) => id.clone(), _ => return Err(contract("Invalid service ID")) }) {
            return Err(contract("Duplicate service ID"));
        }
    }
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
fn instance(value: &JsonValue, wire: bool) -> Result<(), ServiceError> {
    keys(value, &["members"], &["instance"])?;
    if let Some(a) = value.get("instance") {
        address(a)?;
    }
    for member in value["members"]
        .as_array()
        .ok_or_else(|| contract("Invalid service members"))?
    {
        id(&member["name"])?;
        match member["kind"].as_str() {
            Some("method") => keys(member, &["name", "kind"], &[])?,
            Some("state") => {
                keys(member, &["name", "kind", "sequence", "ops"], &[])?;
                operations(member, wire, 0)?;
            }
            _ => return Err(contract("Invalid service member kind")),
        }
    }
    Ok(())
}
fn operations(value: &JsonValue, wire: bool, minimum: u64) -> Result<(), ServiceError> {
    js_integer(&value["sequence"], minimum)?;
    for op in value["ops"]
        .as_array()
        .ok_or_else(|| contract("Invalid service operations"))?
    {
        if wire {
            valid_wire_op(op)?;
        } else {
            valid_op(op)?;
        }
    }
    Ok(())
}
fn snapshot(value: &JsonValue, wire: bool) -> Result<(), ServiceError> {
    keys(value, &["serviceId", "mode", "instances"], &[])?;
    id(&value["serviceId"])?;
    if !matches!(value["mode"].as_str(), Some("singleton" | "keyed")) {
        return Err(contract("Invalid service mode"));
    }
    for v in value["instances"]
        .as_array()
        .ok_or_else(|| contract("Invalid service instances"))?
    {
        instance(v, wire)?;
    }
    Ok(())
}
fn update(value: &JsonValue, wire: bool) -> Result<(), ServiceError> {
    match value["type"].as_str() {
        Some("state") => {
            keys(value, &["type", "member", "sequence", "ops"], &["instance"])?;
            id(&value["member"])?;
            if let Some(a) = value.get("instance") {
                address(a)?;
            }
            operations(value, wire, 1)?;
        }
        Some("reset") => {
            keys(value, &["type", "snapshot"], &[])?;
            snapshot(&value["snapshot"], wire)?;
            if let Some(instances) = value["snapshot"]["instances"].as_array() {
                for i in instances {
                    if let Some(members) = i["members"].as_array() {
                        for m in members {
                            if m["kind"].as_str() == Some("state")
                                && (m["ops"].as_array().map(Vec::len) != Some(1)
                                    || m["ops"][0][0].as_str() != Some("r"))
                            {
                                return Err(contract(
                                    "Service reset must contain full root replacements",
                                ));
                            }
                        }
                    }
                }
            }
        }
        Some("unavailable") => keys(value, &["type"], &[])?,
        Some("replaced") => {
            keys(value, &["type", "snapshot"], &[])?;
            instance(&value["snapshot"], wire)?;
        }
        Some("spawned") => {
            keys(value, &["type", "instance"], &[])?;
            instance(&value["instance"], wire)?;
        }
        Some("closed") => {
            keys(value, &["type", "instance"], &[])?;
            address(&value["instance"])?;
        }
        _ => return Err(contract("Invalid service provider update")),
    }
    Ok(())
}
pub fn parse_service_subscription_snapshot(
    value: impl Into<JsonValue>,
) -> Result<ServiceSubscriptionSnapshot, ServiceError> {
    let value = value.into();
    snapshot(&value, false)?;
    let value = normalize_snapshot(&value)?;
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
pub fn parse_wire_service_subscription_snapshot(
    value: impl Into<JsonValue>,
) -> Result<WireServiceSubscriptionSnapshot, ServiceError> {
    let value = value.into();
    snapshot(&value, true)?;
    let value = normalize_snapshot(&value)?;
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
pub fn parse_service_provider_update(
    value: impl Into<JsonValue>,
) -> Result<ServiceProviderUpdate, ServiceError> {
    let value = value.into();
    update(&value, false)?;
    let value = normalize_update(&value)?;
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
pub fn parse_wire_service_provider_update(
    value: impl Into<JsonValue>,
) -> Result<WireServiceProviderUpdate, ServiceError> {
    let value = value.into();
    update(&value, true)?;
    let value = normalize_update(&value)?;
    Ok(serde_json::from_str(&serde_json::to_string(&value)?)?)
}
