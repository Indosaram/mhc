//! Port of senpi packages/ai/src/endpoint-reasoning-efforts.ts.
//!
//! Reasoning levels an OpenAI-compatible endpoint advertises for one model through the
//! `reasoning_efforts` field of its `/models` listing (senpi#2196).

use crate::types::{ModelThinkingLevel, ThinkingLevelMap};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EndpointReasoningEfforts {
    /// senpi level -> the endpoint's own spelling, sent on the wire as-is. Every level the endpoint
    /// did not advertise is `None`, so the map is authoritative and no level is inferred from the id.
    /// Absent when the endpoint advertised a ladder but no value in it names a senpi level.
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Level of the entry the endpoint marks `default: true`, when that entry maps.
    pub default_thinking_level: Option<ModelThinkingLevel>,
    /// Advertised values that name no senpi level; reported, never sent.
    pub unmapped: Vec<String>,
}

// A closed match, not an object lookup: an advertised "__proto__" or "constructor" must not resolve.
fn level_by_name(name: &str) -> Option<ModelThinkingLevel> {
    Some(match name {
        "none" | "off" => ModelThinkingLevel::Off,
        "minimal" => ModelThinkingLevel::Minimal,
        "low" => ModelThinkingLevel::Low,
        "medium" => ModelThinkingLevel::Medium,
        "high" => ModelThinkingLevel::High,
        "xhigh" => ModelThinkingLevel::Xhigh,
        "max" => ModelThinkingLevel::Max,
        _ => return None,
    })
}

fn read_entry(entry: &Value) -> Option<(String, bool)> {
    if let Some(value) = entry.as_str() {
        return (!value.trim().is_empty()).then(|| (value.to_owned(), false));
    }
    let object = entry.as_object()?;
    let value = object.get("value")?.as_str()?;
    let is_default = object.get("default") == Some(&Value::Bool(true));
    (!value.trim().is_empty()).then(|| (value.to_owned(), is_default))
}

/// Map a `reasoning_efforts` listing (`[{ "value": "low" }, { "value": "high", "default": true }]`,
/// or plain strings) onto senpi's levels. Matching is case-insensitive; the first spelling of a
/// level wins. Returns `None` only when nothing was advertised (the value is not an array); an
/// advertised ladder with no representable value yields no map.
pub fn parse_endpoint_reasoning_efforts(value: &Value) -> Option<EndpointReasoningEfforts> {
    let entries = value.as_array()?;
    let mut advertised: Vec<(ModelThinkingLevel, String)> = Vec::new();
    let mut unmapped: Vec<String> = Vec::new();
    let mut default_thinking_level = None;
    for raw in entries {
        let Some((value, is_default)) = read_entry(raw) else {
            continue;
        };
        let Some(level) = level_by_name(&value.trim().to_lowercase()) else {
            if !unmapped.contains(&value) {
                unmapped.push(value);
            }
            continue;
        };
        if !advertised.iter().any(|(known, _)| *known == level) {
            advertised.push((level, value));
        }
        if is_default && default_thinking_level.is_none() {
            default_thinking_level = Some(level);
        }
    }
    if advertised.is_empty() {
        return Some(EndpointReasoningEfforts {
            unmapped,
            ..Default::default()
        });
    }
    let mut thinking_level_map = ThinkingLevelMap::new();
    for level in ModelThinkingLevel::ALL {
        let spelling = advertised
            .iter()
            .find(|(known, _)| *known == level)
            .map(|(_, value)| value.clone());
        thinking_level_map.insert(level, spelling);
    }
    Some(EndpointReasoningEfforts {
        thinking_level_map: Some(thinking_level_map),
        default_thinking_level,
        unmapped,
    })
}

#[cfg(test)]
#[path = "endpoint_reasoning_efforts_tests.rs"]
mod tests;
