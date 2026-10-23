//! Cycle records written by pre-merge builds of #10548 (never released) carried the render inline:
//! `texts` (key -> rendered text, from stage rendered on) and `progress` (key -> { text, done } at
//! stage merged), with no render journal. Such a record is read here so a resumed cycle still
//! adopts the renders that landed as baselines (apply invariant 5), instead of capturing them back
//! as this device's own edits.
//!
//! This is weaker than invariant 5. Those records never stored the doc's frontier, so adoption can
//! only check that the doc still renders to the recorded text, not that it is still at the version
//! the render came from. Only keys the store already knows are adopted, so a damaged or
//! hand-edited record cannot create an item (#10620).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::engine_deps::EngineDeps;
use super::error::EngineResult;
use super::journal::{CycleRecord, CycleStage};

struct LegacyRender {
    text: Option<String>,
    /// True when the record proves the render landed; otherwise only a matching working file does.
    landed: bool,
}

/// `Some(None)` is a null text, `Some(Some(_))` a string, `None` anything else.
fn as_text(value: &Value) -> Option<Option<String>> {
    match value {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    }
}

fn legacy_renders(cycle: &CycleRecord) -> BTreeMap<String, LegacyRender> {
    let mut renders = BTreeMap::new();
    if let Some(Value::Object(progress)) = &cycle.progress {
        for (key, step) in progress {
            let Value::Object(step) = step else { continue };
            let Some(text) = step.get("text").and_then(as_text) else {
                continue;
            };
            let landed = step.get("done") == Some(&Value::Bool(true));
            renders.insert(key.clone(), LegacyRender { text, landed });
        }
    }
    if let Some(Value::Object(texts)) = &cycle.texts
        && cycle.stage != CycleStage::Merged
    {
        for (key, text) in texts {
            if let Some(text) = as_text(text) {
                renders.insert(key.clone(), LegacyRender { text, landed: true });
            }
        }
    }
    renders
}

/// Adopts each legacy render of a key without a render record while the doc still renders to it.
pub fn adopt_legacy_renders(
    deps: &EngineDeps,
    cycle: &CycleRecord,
    recorded: &BTreeSet<String>,
    read_working: &dyn Fn(&str) -> EngineResult<Option<String>>,
) -> EngineResult<()> {
    let known: BTreeSet<String> = deps.store.keys().into_iter().collect();
    for (key, render) in legacy_renders(cycle) {
        if recorded.contains(&key) || !known.contains(&key) {
            continue;
        }
        if deps.store.render(&key, render.text.as_deref())? != render.text {
            continue;
        }
        if !render.landed && read_working(&key)? != render.text {
            continue;
        }
        deps.store.mark_materialized(&key, render.text.as_deref())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "legacy_cycle_tests.rs"]
mod tests;
