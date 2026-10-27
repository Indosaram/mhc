//! The disabled-skills union across layers (spec §9).

use crate::load::{Harness, Layer};
use serde_json::Value;

fn harness_blocks(harness: &Harness) -> Vec<String> {
    match harness {
        Harness::Native => vec!["[native]".into(), "[senpi]".into()],
        Harness::Other(name) => vec![format!("[{name}]")],
    }
}

fn collect_from(container: &Value, blocks: &[String], out: &mut Vec<String>) {
    let mut scopes = vec![container];
    for block in blocks {
        if let Some(inner) = container.get(block.as_str()) {
            scopes.push(inner);
        }
    }
    for scope in scopes {
        let Some(entries) = scope.get("disabled_skills").and_then(Value::as_array) else {
            continue;
        };
        for entry in entries {
            let Some(name) = entry.as_str().map(str::trim).filter(|n| !n.is_empty()) else {
                continue;
            };
            if !out.iter().any(|seen| seen == name) {
                out.push(name.to_owned());
            }
        }
    }
}

/// Every disabled skill named by any layer (first-seen order, no duplicates). A later layer
/// cannot re-enable a skill an earlier layer disabled.
pub fn collect_disabled_skills(
    layers: &[Layer],
    harness: &Harness,
    profile: Option<&str>,
) -> Vec<String> {
    let blocks = harness_blocks(harness);
    let mut out = Vec::new();
    for layer in layers {
        collect_from(&layer.value, &blocks, &mut out);
        if let Some(name) = profile
            && let Some(body) = layer.value.get("profiles").and_then(|p| p.get(name))
        {
            collect_from(body, &blocks, &mut out);
        }
    }
    out
}
