//! Port of `cache-keepalive/prewarm-entry.ts` (senpi 903b8336).

use maho_ai::types::{Usage, UsageCost};
use serde_json::{Value, json};

pub const PROMPT_CACHE_PREWARM_ENTRY_TYPE: &str = "prompt-cache-prewarm";

/// `PromptCachePrewarmEntryData`: the custom entry a session prewarm appends.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptCachePrewarmEntryData {
    Warmed {
        provider: String,
        model: String,
        usage: Usage,
    },
    Failed {
        provider: String,
        model: String,
        error: String,
    },
    Skipped {
        provider: String,
        model: String,
        reason: String,
    },
}

impl PromptCachePrewarmEntryData {
    /// The entry `data` as upstream writes it: `{ phase, provider, model, usage | error | reason }`.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Warmed {
                provider,
                model,
                usage,
            } => {
                json!({"phase":"warmed","provider":provider,"model":model,"usage":serde_json::to_value(usage).unwrap_or(Value::Null)})
            }
            Self::Failed {
                provider,
                model,
                error,
            } => json!({"phase":"failed","provider":provider,"model":model,"error":error}),
            Self::Skipped {
                provider,
                model,
                reason,
            } => json!({"phase":"skipped","provider":provider,"model":model,"reason":reason}),
        }
    }
}

fn finite(value: &Value, key: &str) -> Option<f64> {
    value.get(key)?.as_f64().filter(|number| number.is_finite())
}

/// `isUsage`: input/output/cacheRead/cacheWrite and cost.total must be finite numbers.
/// Rust `Usage` holds unsigned token counts, so a finite negative or fractional count saturates
/// toward zero / truncates instead of passing through as upstream's JS number would.
fn usage_from(value: &Value) -> Option<Usage> {
    if !value.is_object() {
        return None;
    }
    let input = finite(value, "input")?;
    let output = finite(value, "output")?;
    let cache_read = finite(value, "cacheRead")?;
    let cache_write = finite(value, "cacheWrite")?;
    let cost = value.get("cost").filter(|cost| cost.is_object())?;
    let total = finite(cost, "total")?;
    let tokens = |number: f64| number.max(0.0) as u64;
    Some(Usage {
        input: tokens(input),
        output: tokens(output),
        cache_read: tokens(cache_read),
        cache_write: tokens(cache_write),
        total_tokens: finite(value, "totalTokens").map_or(0, tokens),
        cost: UsageCost {
            input: finite(cost, "input").unwrap_or_default(),
            output: finite(cost, "output").unwrap_or_default(),
            cache_read: finite(cost, "cacheRead").unwrap_or_default(),
            cache_write: finite(cost, "cacheWrite").unwrap_or_default(),
            total,
        },
        ..Default::default()
    })
}

/// `getPromptCachePrewarmUsage`: the billed usage of a `warmed` prewarm custom entry.
pub fn get_prompt_cache_prewarm_usage(entry: &Value) -> Option<Usage> {
    if entry.get("type").and_then(Value::as_str) != Some("custom")
        || entry.get("customType").and_then(Value::as_str) != Some(PROMPT_CACHE_PREWARM_ENTRY_TYPE)
    {
        return None;
    }
    let data = entry.get("data")?;
    if data.get("phase").and_then(Value::as_str) != Some("warmed") {
        return None;
    }
    usage_from(data.get("usage")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_json() -> Value {
        json!({"input":10,"output":2,"cacheRead":30,"cacheWrite":40,"totalTokens":82,"cost":{"input":0.1,"output":0.2,"cacheRead":0.3,"cacheWrite":0.4,"total":1.0}})
    }

    fn entry(data: Value) -> Value {
        json!({"type":"custom","customType":PROMPT_CACHE_PREWARM_ENTRY_TYPE,"data":data})
    }

    #[test]
    fn warmed_entry_yields_its_usage() {
        let usage = get_prompt_cache_prewarm_usage(&entry(
            json!({"phase":"warmed","provider":"openai","model":"m","usage":usage_json()}),
        ))
        .expect("usage");
        assert_eq!(
            (
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                usage.total_tokens
            ),
            (10, 2, 30, 40, 82)
        );
        assert_eq!(usage.cost.total, 1.0);
        assert_eq!(usage.cost.cache_write, 0.4);
    }

    #[test]
    fn other_entries_and_phases_have_no_usage() {
        assert!(
            get_prompt_cache_prewarm_usage(&entry(json!({"phase":"failed","usage":usage_json()})))
                .is_none()
        );
        assert!(get_prompt_cache_prewarm_usage(&entry(json!({"phase":"skipped"}))).is_none());
        assert!(get_prompt_cache_prewarm_usage(&json!({"type":"custom","customType":"other","data":{"phase":"warmed","usage":usage_json()}})).is_none());
        assert!(
            get_prompt_cache_prewarm_usage(
                &json!({"type":"message","customType":PROMPT_CACHE_PREWARM_ENTRY_TYPE})
            )
            .is_none()
        );
        assert!(
            get_prompt_cache_prewarm_usage(
                &json!({"type":"custom","customType":PROMPT_CACHE_PREWARM_ENTRY_TYPE})
            )
            .is_none()
        );
    }

    #[test]
    fn malformed_usage_is_rejected() {
        for usage in [
            json!(null),
            json!({"input":1,"output":1,"cacheRead":1,"cacheWrite":1}),
            json!({"input":"1","output":1,"cacheRead":1,"cacheWrite":1,"cost":{"total":0}}),
            json!({"input":1,"output":1,"cacheRead":1,"cacheWrite":1,"cost":null}),
            json!({"input":1,"output":1,"cacheRead":1,"cacheWrite":1,"cost":{}}),
        ] {
            assert!(
                get_prompt_cache_prewarm_usage(&entry(json!({"phase":"warmed","usage":usage})))
                    .is_none()
            );
        }
    }

    #[test]
    fn entry_data_serializes_with_phase_tag() {
        let data = PromptCachePrewarmEntryData::Skipped {
            provider: "p".into(),
            model: "m".into(),
            reason: "r".into(),
        };
        assert_eq!(
            data.to_json(),
            json!({"phase":"skipped","provider":"p","model":"m","reason":"r"})
        );
        let failed = PromptCachePrewarmEntryData::Failed {
            provider: "p".into(),
            model: "m".into(),
            error: "e".into(),
        };
        assert_eq!(failed.to_json()["phase"], "failed");
    }
}
