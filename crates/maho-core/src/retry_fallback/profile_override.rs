use indexmap::IndexMap;
use serde_json::{Map,Value};
use std::collections::HashSet;

#[derive(Debug,Default)]
pub struct ValidatedRetryProviderOverrides {pub overrides:IndexMap<String,Value>,pub warnings:Vec<String>}

fn describe(value:&Value) -> &'static str {match value {Value::Null=>"null",Value::Array(_)=>"an array",Value::Bool(_)=>"a boolean",Value::Number(_)=>"a number",Value::String(_)=>"a string",Value::Object(_)=>"an object"}}
fn safe_integer(value:&Value) -> bool {value.as_f64().is_some_and(|v|(0.0..=9_007_199_254_740_991.0).contains(&v)&&v.fract()==0.0)}
fn validate_stage(value:&Value,id:&str,stage:&str,warnings:&mut Vec<String>) -> Option<Value> {
    let Some(object)=value.as_object() else {warnings.push(format!("retry.providers.{id}.{stage} must be a plain object, but got {}.",describe(value)));return None;};
    let mut out=Map::new();let mut invalid=Vec::new();
    for key in ["enabled","maxRetries","baseDelayMs","growthFactor","perAttemptCapMs","jitter","serverHintMaxDelayMs"] {
        let Some(value)=object.get(key) else {continue;};
        let valid=match key {
            "enabled"=>value.is_boolean(),
            "growthFactor"=>value.as_f64().is_some_and(|v|v.is_finite()&&v>=1.0),
            "perAttemptCapMs"|"serverHintMaxDelayMs"=>value.is_null()||safe_integer(value),
            "jitter"=>match value.get("mode").and_then(Value::as_str) {Some("none")=>true,Some("additive"|"subtractive")=>value.get("ratio").and_then(Value::as_f64).is_some_and(|v|v.is_finite()&&(0.0..=1.0).contains(&v)),_=>false},
            _=>safe_integer(value),
        };
        if valid {out.insert(key.into(),if key=="jitter"&&value.get("mode").and_then(Value::as_str)==Some("none") {serde_json::json!({"mode":"none"})} else {value.clone()});} else {invalid.push(key);}
    }
    if invalid.is_empty() {Some(Value::Object(out))} else {warnings.push(format!("retry.providers.{id}.{stage} has invalid knob(s): {}.",invalid.join(", ")));None}
}

pub fn validate_retry_provider_overrides(value:Option<&Value>,known:&HashSet<String>,tiered:Option<&HashSet<String>>) -> ValidatedRetryProviderOverrides {
    let mut result=ValidatedRetryProviderOverrides::default();let Some(value)=value else {return result;};
    let Some(entries)=value.as_object() else {result.warnings.push(format!("retry.providers must be a plain object keyed by provider id, but got {}.",describe(value)));return result;};
    for (id,value) in entries {
        if !known.contains(id) {result.warnings.push(format!("retry.providers.{id}: unknown provider id \"{id}\"."));continue;}
        let Some(entry)=value.as_object() else {result.warnings.push(format!("retry.providers.{id} must be a plain object, but got {}.",describe(value)));continue;};
        let start=result.warnings.len();let mut stages=Map::new();
        for stage in ["providerRequest","turn"] {if let Some(value)=entry.get(stage)&&let Some(value)=validate_stage(value,id,stage,&mut result.warnings){stages.insert(stage.into(),value);}}
        if result.warnings.len()!=start {continue;}
        if tiered.is_some_and(|v|v.contains(id))&&stages.values().any(|v|v.get("serverHintMaxDelayMs").is_some()) {result.warnings.push(format!("retry.providers.{id}: serverHintMaxDelayMs is invalid for a tiered stage (tier thresholds own that strategy)."));continue;}
        if stages.is_empty(){result.warnings.push(format!("retry.providers.{id} has no recognized knobs."));continue;}
        result.overrides.insert(id.clone(),Value::Object(stages));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_knob_rejects_entire_provider() {
        let value=serde_json::json!({"p":{"providerRequest":{"maxRetries":3},"turn":{"growthFactor":0.5}}});
        let result=validate_retry_provider_overrides(Some(&value),&HashSet::from(["p".into()]),None);
        assert!(result.overrides.is_empty());assert_eq!(result.warnings.len(),1);
    }
    #[test]
    fn null_caps_are_preserved() {
        let value=serde_json::json!({"p":{"turn":{"perAttemptCapMs":null,"maxRetries":0}}});
        let result=validate_retry_provider_overrides(Some(&value),&HashSet::from(["p".into()]),None);
        assert!(result.warnings.is_empty());assert!(result.overrides["p"]["turn"]["perAttemptCapMs"].is_null());
    }
}
