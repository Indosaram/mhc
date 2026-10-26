//! Durable, idempotent held human inputs.
use indexmap::IndexMap;
use serde_json::{Value, json};
pub const HELD_INPUT_ENTRY: &str = "held-input";
pub const HELD_INPUT_WITHDRAWN_ENTRY: &str = "held-input-withdrawn";
pub const HELD_INPUTS_CONSUMED_ENTRY: &str = "held-inputs-consumed";
pub fn held_input_history(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| {
            e["type"] == "custom"
                && [
                    HELD_INPUT_ENTRY,
                    HELD_INPUT_WITHDRAWN_ENTRY,
                    HELD_INPUTS_CONSUMED_ENTRY,
                ]
                .contains(&e["customType"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect()
}
fn is_held_input_data(v: &Value) -> bool {
    v["inputId"].is_string()
        && v["revision"].is_number()
        && v["text"].is_string()
        && v.get("prepare").is_none_or(|p| p == true)
        && v.get("images").is_none_or(|imgs| {
            imgs.as_array().is_some_and(|a| {
                a.iter().all(|i| {
                    i["type"] == "image" && i["data"].is_string() && i["mimeType"].is_string()
                })
            })
        })
}
pub struct HeldInputs {
    inputs: IndexMap<String, Value>,
    settled: IndexMap<String, Value>,
}
impl HeldInputs {
    pub fn new(entries: &[Value]) -> Self {
        let mut s = Self {
            inputs: IndexMap::new(),
            settled: IndexMap::new(),
        };
        for e in entries {
            if e["type"] != "custom" {
                continue;
            }
            if e["customType"] == HELD_INPUT_ENTRY && is_held_input_data(&e["data"]) {
                let mut input = e["data"].clone();
                input["entryId"] = e["id"].clone();
                s.inputs.insert(
                    input["inputId"].as_str().expect("validated id").into(),
                    input,
                );
            } else if [HELD_INPUT_WITHDRAWN_ENTRY, HELD_INPUTS_CONSUMED_ENTRY]
                .contains(&e["customType"].as_str().unwrap_or(""))
            {
                for i in e["data"]["inputs"].as_array().into_iter().flatten() {
                    if i["inputId"].is_string() && i["revision"].is_number() {
                        s.settle(i);
                    }
                }
            }
        }
        s
    }
    pub fn hold(
        &mut self,
        input: Value,
        append: impl FnOnce(&str, &Value) -> Result<String, String>,
    ) -> Result<Value, String> {
        if !is_held_input_data(&input) {
            return Err("Invalid held input".into());
        }
        let id = input["inputId"].as_str().expect("validated id");
        if let Some(existing) = self.inputs.get(id) {
            return Ok(if existing["revision"] != input["revision"] {
                json!({"kind":"input_id_conflict","heldRevision":existing["revision"]})
            } else {
                json!({"kind":"held","entryId":existing["entryId"],"duplicate":true})
            });
        }
        if let Some(revision) = self.settled.get(id) {
            return Ok(if revision == &input["revision"] {
                json!({"kind":"already_delivered","revision":revision})
            } else {
                json!({"kind":"input_id_conflict","heldRevision":revision})
            });
        }
        let entry_id = append(HELD_INPUT_ENTRY, &input)?;
        let id = id.to_owned();
        let mut input = input;
        input["entryId"] = json!(entry_id);
        self.inputs.insert(id, input);
        Ok(json!({"kind":"held","entryId":entry_id,"duplicate":false}))
    }
    pub fn withdraw(
        &mut self,
        id: &str,
        append: impl FnOnce(&str, &Value) -> Result<String, String>,
    ) -> Result<bool, String> {
        let Some(i) = self.inputs.get(id) else {
            return Ok(false);
        };
        let settled = json!({"inputId":id,"revision":i["revision"]});
        append(HELD_INPUT_WITHDRAWN_ENTRY, &json!({"inputs":[settled]}))?;
        self.settle(&settled);
        Ok(true)
    }
    pub fn pending(&self) -> Vec<Value> {
        self.inputs.values().cloned().collect()
    }
    pub fn consume(
        &mut self,
        ids: &[String],
        append: impl FnOnce(&str, &Value) -> Result<String, String>,
    ) -> Result<(), String> {
        let held = ids
            .iter()
            .filter_map(|id| {
                self.inputs
                    .get(id)
                    .map(|i| json!({"inputId":id,"revision":i["revision"]}))
            })
            .collect::<Vec<_>>();
        if held.is_empty() {
            return Ok(());
        }
        append(HELD_INPUTS_CONSUMED_ENTRY, &json!({"inputs":held}))?;
        for i in &held {
            self.settle(i);
        }
        Ok(())
    }
    fn settle(&mut self, i: &Value) {
        if let Some(id) = i["inputId"].as_str() {
            self.inputs.shift_remove(id);
            self.settled.insert(id.into(), i["revision"].clone());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_deduplicate_and_consumption_is_durable() {
        let mut h = HeldInputs::new(&[]);
        let i = json!({"inputId":"i","revision":1,"text":"/command","prepare":true});
        assert_eq!(
            h.hold(i.clone(), |_, _| Ok("e".into())).expect("hold")["duplicate"],
            false
        );
        assert_eq!(
            h.hold(i.clone(), |_, _| panic!("duplicate write"))
                .expect("duplicate")["duplicate"],
            true
        );
        h.consume(&["i".into()], |_, _| Ok("c".into()))
            .expect("consume");
        assert_eq!(
            h.hold(i, |_, _| panic!("redelivery write"))
                .expect("settled")["kind"],
            "already_delivered"
        );
    }
}

#[cfg(test)]
mod upstream_reopen_tests {
    use super::*;
    #[test]
    fn held_revision_conflicts_and_withdrawn_consumed_survive_reopen() {
        let mut entries = Vec::new();
        let mut append = |kind: &str, data: &Value| {
            let id = format!("e{}", entries.len() + 1);
            entries.push(json!({"type":"custom","customType":kind,"id":id,"data":data}));
            Ok(id)
        };
        let mut held = HeldInputs::new(&[]);
        for id in ["a", "b", "c"] {
            held.hold(json!({"inputId":id,"revision":1,"text":id}), &mut append)
                .expect("hold");
        }
        assert_eq!(
            held.hold(
                json!({"inputId":"a","revision":2,"text":"changed"}),
                &mut append
            )
            .expect("conflict")["kind"],
            "input_id_conflict"
        );
        held.withdraw("b", &mut append).expect("withdraw");
        held.consume(&["c".into()], &mut append).expect("consume");
        let mut reopened = HeldInputs::new(&entries);
        assert_eq!(reopened.pending().len(), 1);
        assert_eq!(reopened.pending()[0]["inputId"], "a");
        assert_eq!(
            reopened
                .hold(
                    json!({"inputId":"c","revision":1,"text":"c"}),
                    |_, _| panic!("redelivery")
                )
                .expect("settled")["kind"],
            "already_delivered"
        );
        assert_eq!(
            reopened
                .hold(
                    json!({"inputId":"c","revision":2,"text":"changed"}),
                    |_, _| panic!("replacement")
                )
                .expect("conflict")["kind"],
            "input_id_conflict"
        );
    }
}
