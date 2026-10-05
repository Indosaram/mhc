//! Wire-only Chord dependencies needed by the client (no facet runtime).
use super::errors::ClientError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

fn invalid(description: &str) -> ClientError {
    ClientError::Protocol(format!("Invalid {description}"))
}
fn id(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty())
}
fn integer(v: &Value, min: f64) -> bool {
    v.as_f64().is_some_and(|n| n.fract() == 0.0 && n >= min)
}
fn keys(
    v: &Value,
    required: &[&str],
    optional: &[&str],
    description: &str,
) -> Result<(), ClientError> {
    if v.as_object().is_some_and(|o| {
        required.iter().all(|k| o.contains_key(*k))
            && o.keys()
                .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    }) {
        Ok(())
    } else {
        Err(invalid(description))
    }
}
fn address(v: &Value) -> Result<(), ClientError> {
    keys(v, &["key", "generation"], &[], "service instance address")?;
    if id(&v["key"]) && integer(&v["generation"], 1.0) {
        Ok(())
    } else {
        Err(invalid("service instance address"))
    }
}
pub fn parse_service_call(v: &Value) -> Result<(), ClientError> {
    keys(
        v,
        &["serviceId", "member", "args"],
        &["instance"],
        "service call",
    )?;
    if !id(&v["serviceId"]) || !id(&v["member"]) || !v["args"].is_array() {
        return Err(invalid("service call"));
    }
    if let Some(instance) = v.get("instance") {
        address(instance)?;
    }
    Ok(())
}
pub fn parse_catalogue(v: &Value) -> Result<(), ClientError> {
    let entries = v.as_array().ok_or_else(|| invalid("service catalogue"))?;
    let mut ids = HashSet::new();
    for entry in entries {
        keys(
            entry,
            &["serviceId", "mode"],
            &[],
            "service catalogue entry",
        )?;
        if !id(&entry["serviceId"])
            || !matches!(entry["mode"].as_str(), Some("singleton" | "keyed"))
            || !ids.insert(entry["serviceId"].clone().to_string())
        {
            return Err(invalid("service catalogue"));
        }
    }
    Ok(())
}
fn path(v: &Value) -> Result<(), ClientError> {
    let p = v
        .as_array()
        .ok_or_else(|| ClientError::Protocol("path is not an array".into()))?;
    for seg in p {
        if let Some(s) = seg.as_str() {
            if matches!(s, "__proto__" | "constructor" | "prototype") {
                return Err(ClientError::Protocol(format!("unsafe path segment: {s}")));
            }
        } else if !integer(seg, 0.0) {
            return Err(ClientError::Protocol(format!("unsafe path segment: {seg}")));
        }
    }
    Ok(())
}
fn reference(v: &Value) -> Result<(), ClientError> {
    if v.is_number() {
        if integer(v, 0.0) {
            Ok(())
        } else {
            Err(ClientError::Protocol("bad path id".into()))
        }
    } else {
        path(v)
    }
}
fn wire_op(v: &Value) -> Result<(), ClientError> {
    let a = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| ClientError::Protocol("op is not a tuple".into()))?;
    let verb = a[0].as_str().unwrap_or_default();
    let valid = match verb {
        "r" => a.len() == 2,
        "s" => {
            if a.len() == 3 {
                reference(&a[1])?;
            }
            matches!(a.len(), 2 | 3)
        }
        "d" => {
            if a.len() == 2 {
                reference(&a[1])?;
            }
            matches!(a.len(), 1 | 2)
        }
        "a" | "t" => {
            if a.len() == 3 {
                reference(&a[1])?;
            }
            matches!(a.len(), 2 | 3)
                && a.last().is_some_and(|v| {
                    if verb == "a" {
                        v.is_string()
                    } else {
                        integer(v, 0.0)
                    }
                })
        }
        "p" => {
            if a.len() == 5 {
                reference(&a[1])?;
            }
            let offset = if a.len() == 5 { 2 } else { 1 };
            matches!(a.len(), 4 | 5)
                && integer(&a[offset], 0.0)
                && integer(&a[offset + 1], 0.0)
                && a[offset + 2].is_array()
        }
        "#" => {
            if a.len() == 3 {
                path(&a[2])?;
            }
            a.len() == 3 && integer(&a[1], 0.0)
        }
        _ => return Err(ClientError::Protocol(format!("unknown op verb: {verb}"))),
    };
    if valid {
        Ok(())
    } else {
        Err(ClientError::Protocol(format!("{verb} shape")))
    }
}

#[derive(Default)]
struct DeltaDecoder {
    paths: BTreeMap<u64, Value>,
}
impl DeltaDecoder {
    fn decode(&mut self, ops: &Value) -> Result<Value, ClientError> {
        let ops = ops
            .as_array()
            .ok_or_else(|| invalid("service state operations"))?;
        let mut previous = None;
        let mut result = Vec::new();
        for op in ops {
            wire_op(op)?;
            let a = op.as_array().ok_or_else(|| invalid("operation"))?;
            let verb = a[0].as_str().unwrap_or_default();
            if verb == "#" {
                let id = a[1]
                    .as_u64()
                    .ok_or_else(|| ClientError::Protocol("bad path id".into()))?;
                self.paths.insert(id, a[2].clone());
                continue;
            }
            if verb == "r" {
                result.push(op.clone());
                self.paths.clear();
                previous = None;
                continue;
            }
            let short = (verb == "d" && a.len() == 1)
                || (verb != "d" && verb != "p" && a.len() == 2)
                || (verb == "p" && a.len() == 4);
            let resolved = if short {
                previous
                    .clone()
                    .ok_or_else(|| ClientError::Protocol("unresolvable path: []".into()))?
            } else {
                let resolved = if let Some(id) = a[1].as_u64() {
                    self.paths
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| ClientError::Protocol(format!("unresolvable path: {id}")))?
                } else {
                    a[1].clone()
                };
                previous = Some(resolved.clone());
                resolved
            };
            if verb != "p" && resolved.as_array().is_some_and(Vec::is_empty) {
                return Err(ClientError::Protocol("unresolvable path: []".into()));
            }
            let mut decoded = vec![a[0].clone(), resolved];
            decoded.extend_from_slice(&a[if short { 1 } else { 2 }..]);
            result.push(Value::Array(decoded));
        }
        Ok(Value::Array(result))
    }
}

#[derive(Default)]
pub struct ServiceStateDecoder {
    codecs: BTreeMap<String, DeltaDecoder>,
}
fn state_key(instance: Option<&Value>, member: &Value) -> String {
    json!([
        instance.map(|v| &v["key"]),
        instance.map(|v| &v["generation"]),
        member
    ])
    .to_string()
}
impl ServiceStateDecoder {
    fn instance(&mut self, v: &Value) -> Result<Value, ClientError> {
        keys(v, &["members"], &["instance"], "service instance snapshot")?;
        if let Some(instance) = v.get("instance") {
            address(instance)?;
        }
        let members = v["members"]
            .as_array()
            .ok_or_else(|| invalid("service instance snapshot"))?;
        let mut decoded = v.clone();
        let mut decoded_members = Vec::new();
        for member in members {
            let mut output = member.clone();
            match member["kind"].as_str() {
                Some("method") => {
                    keys(member, &["name", "kind"], &[], "service method snapshot")?;
                    if !id(&member["name"]) {
                        return Err(invalid("service method snapshot"));
                    }
                }
                Some("state") => {
                    keys(
                        member,
                        &["name", "kind", "sequence", "ops"],
                        &[],
                        "service state snapshot",
                    )?;
                    if !id(&member["name"]) || !integer(&member["sequence"], 0.0) {
                        return Err(invalid("service state snapshot"));
                    }
                    let key = state_key(v.get("instance"), &member["name"]);
                    if self.codecs.contains_key(&key) {
                        return Err(ClientError::Protocol(format!(
                            "Duplicate service state {}",
                            member["name"]
                        )));
                    }
                    let mut codec = DeltaDecoder::default();
                    output["ops"] = codec.decode(&member["ops"])?;
                    self.codecs.insert(key, codec);
                }
                _ => return Err(invalid("service member snapshot")),
            }
            decoded_members.push(output);
        }
        decoded["members"] = Value::Array(decoded_members);
        Ok(decoded)
    }
    pub fn snapshot(&mut self, v: &Value) -> Result<Value, ClientError> {
        keys(
            v,
            &["serviceId", "mode", "instances"],
            &[],
            "service subscription snapshot",
        )?;
        if !id(&v["serviceId"]) || !matches!(v["mode"].as_str(), Some("singleton" | "keyed")) {
            return Err(invalid("service subscription snapshot"));
        }
        let instances = v["instances"]
            .as_array()
            .ok_or_else(|| invalid("service subscription snapshot"))?;
        self.codecs.clear();
        let mut output = v.clone();
        let mut decoded = Vec::new();
        for instance in instances {
            decoded.push(self.instance(instance)?);
        }
        output["instances"] = Value::Array(decoded);
        Ok(output)
    }
    pub fn update(&mut self, v: &Value) -> Result<Value, ClientError> {
        let mut output = v.clone();
        match v["type"].as_str() {
            Some("state") => {
                keys(
                    v,
                    &["type", "member", "sequence", "ops"],
                    &["instance"],
                    "state update",
                )?;
                if !id(&v["member"]) || !integer(&v["sequence"], 1.0) {
                    return Err(invalid("service state update"));
                }
                if let Some(instance) = v.get("instance") {
                    address(instance)?;
                }
                let key = state_key(v.get("instance"), &v["member"]);
                let codec = self.codecs.get_mut(&key).ok_or_else(|| {
                    ClientError::Protocol(format!("Unknown service state {}", v["member"]))
                })?;
                output["ops"] = codec.decode(&v["ops"])?;
            }
            Some("unavailable") => {
                keys(v, &["type"], &[], "unavailable update")?;
                self.codecs.clear();
            }
            Some("replaced") => {
                keys(v, &["type", "snapshot"], &[], "replacement update")?;
                self.codecs.clear();
                output["snapshot"] = self.instance(&v["snapshot"])?;
            }
            Some("spawned") => {
                keys(v, &["type", "instance"], &[], "spawn update")?;
                output["instance"] = self.instance(&v["instance"])?;
            }
            Some("closed") => {
                keys(v, &["type", "instance"], &[], "close update")?;
                address(&v["instance"])?;
                let key = &v["instance"]["key"];
                let generation = &v["instance"]["generation"];
                self.codecs.retain(|k, _| {
                    let parsed: Result<Value, _> = serde_json::from_str(k);
                    !parsed.is_ok_and(|v| &v[0] == key && &v[1] == generation)
                });
            }
            _ => return Err(invalid("service provider update")),
        }
        Ok(output)
    }
}
