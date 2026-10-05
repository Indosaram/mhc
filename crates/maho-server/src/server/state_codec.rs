use super::errors::ServerError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct DeltaEncoder {
    seen: BTreeSet<String>,
    ids: BTreeMap<String, u64>,
    next_id: u64,
}
impl DeltaEncoder {
    fn encode(&mut self, ops: &Value) -> Result<Value, ServerError> {
        let ops = ops
            .as_array()
            .ok_or_else(|| ServerError::new("invalid_request", "Invalid state operations"))?;
        let mut previous = None;
        let mut output = Vec::new();
        for op in ops {
            let values = op
                .as_array()
                .ok_or_else(|| ServerError::new("invalid_request", "Invalid state operation"))?;
            let verb = values
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| ServerError::new("invalid_request", "Invalid state operation"))?;
            if verb == "r" {
                output.push(op.clone());
                self.seen.clear();
                self.ids.clear();
                self.next_id = 0;
                previous = None;
                continue;
            }
            let path = values
                .get(1)
                .filter(|v| v.is_array())
                .ok_or_else(|| ServerError::new("invalid_request", "Invalid operation path"))?;
            let key = path.to_string();
            if previous.as_ref() == Some(&key) {
                let mut encoded = vec![values[0].clone()];
                encoded.extend_from_slice(&values[2..]);
                output.push(Value::Array(encoded));
                continue;
            }
            let reference = if let Some(id) = self.ids.get(&key) {
                json!(id)
            } else if self.seen.contains(&key) {
                let id = self.next_id;
                self.next_id += 1;
                self.ids.insert(key.clone(), id);
                output.push(json!(["#", id, path]));
                json!(id)
            } else {
                self.seen.insert(key.clone());
                path.clone()
            };
            let mut encoded = vec![values[0].clone(), reference];
            encoded.extend_from_slice(&values[2..]);
            output.push(Value::Array(encoded));
            previous = Some(key);
        }
        Ok(Value::Array(output))
    }
}
#[derive(Default)]
pub struct ServiceStateEncoder {
    codecs: BTreeMap<String, DeltaEncoder>,
}
fn key(instance: Option<&Value>, member: &Value) -> String {
    json!([
        instance.map(|v| &v["key"]),
        instance.map(|v| &v["generation"]),
        member
    ])
    .to_string()
}
impl ServiceStateEncoder {
    fn instance(&mut self, instance: &Value) -> Result<Value, ServerError> {
        let mut output = instance.clone();
        let members = instance["members"].as_array().ok_or_else(|| {
            ServerError::new("invalid_request", "Invalid service instance snapshot")
        })?;
        let mut encoded = Vec::new();
        for member in members {
            let mut output = member.clone();
            if member["kind"] == "state" {
                let key = key(instance.get("instance"), &member["name"]);
                if self.codecs.contains_key(&key) {
                    return Err(ServerError::new(
                        "invalid_request",
                        "Duplicate service state",
                    ));
                }
                let mut codec = DeltaEncoder::default();
                output["ops"] = codec.encode(&member["ops"])?;
                self.codecs.insert(key, codec);
            }
            encoded.push(output);
        }
        output["members"] = Value::Array(encoded);
        Ok(output)
    }
    pub fn snapshot(&mut self, snapshot: &Value) -> Result<Value, ServerError> {
        self.codecs.clear();
        let mut output = snapshot.clone();
        let instances = snapshot["instances"].as_array().ok_or_else(|| {
            ServerError::new("invalid_request", "Invalid service subscription snapshot")
        })?;
        output["instances"] = Value::Array(
            instances
                .iter()
                .map(|i| self.instance(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
        crate::client::service_wire::ServiceStateDecoder::default()
            .snapshot(&output)
            .map_err(|e| ServerError::new("invalid_request", &e.to_string()))?;
        Ok(output)
    }
    pub fn update(&mut self, update: &Value) -> Result<Value, ServerError> {
        let mut output = update.clone();
        match update["type"].as_str() {
            Some("state") => {
                let key = key(update.get("instance"), &update["member"]);
                let codec = self
                    .codecs
                    .get_mut(&key)
                    .ok_or_else(|| ServerError::new("invalid_request", "Unknown service state"))?;
                output["ops"] = codec.encode(&update["ops"])?;
            }
            Some("replaced") => {
                self.codecs.clear();
                output["snapshot"] = self.instance(&update["snapshot"])?;
            }
            Some("spawned") => output["instance"] = self.instance(&update["instance"])?,
            Some("unavailable") => self.codecs.clear(),
            Some("closed") => {
                self.codecs.retain(|k, _| {
                    let parsed: Result<Value, _> = serde_json::from_str(k);
                    !parsed.is_ok_and(|v| {
                        v[0] == update["instance"]["key"]
                            && v[1] == update["instance"]["generation"]
                    })
                });
            }
            _ => {
                return Err(ServerError::new(
                    "invalid_request",
                    "Invalid service provider update",
                ));
            }
        }
        Ok(output)
    }
}
