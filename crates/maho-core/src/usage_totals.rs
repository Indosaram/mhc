//! Port of senpi packages/coding-agent/src/core/usage-totals.ts.

use std::collections::BTreeMap;

use maho_ai::types::Usage;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost: f64,
    pub cost_unknown: bool,
}

pub fn create_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

pub fn add_usage_to_totals(totals: &mut UsageTotals, usage: &Usage) {
    totals.input += usage.input as i64;
    totals.output += usage.output as i64;
    totals.cache_read += usage.cache_read as i64;
    totals.cache_write += usage.cache_write as i64;
    totals.cost += usage.cost.total;
    if usage.cost_unknown == Some(true) {
        totals.cost_unknown = true;
    }
}

pub fn combine_usage(first: Usage, second: Usage) -> Usage {
    maho_agent::harness::utils::usage::add_usage(&first, &second)
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageCostBreakdownEntry {
    pub key: String,
    pub cost: f64,
    pub cost_unknown: bool,
    pub tokens: i64,
}

fn usage_from(value: &Value) -> Option<Usage> {
    serde_json::from_value(value.get("usage")?.clone()).ok()
}

fn prewarm_usage(entry: &Value) -> Option<Usage> {
    let raw = &entry["data"]["usage"];
    let number = |key: &str| {
        raw.get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
    };
    let input = number("input")? as u64;
    let output = number("output")? as u64;
    let cache_read = number("cacheRead")? as u64;
    let cache_write = number("cacheWrite")? as u64;
    let total = raw
        .get("cost")?
        .get("total")?
        .as_f64()
        .filter(|n| n.is_finite())?;
    Some(Usage {
        input,
        output,
        cache_read,
        cache_write,
        cost_unknown: raw["costUnknown"].as_bool().filter(|unknown| *unknown),
        cost: maho_ai::types::UsageCost {
            total,
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Groups attributable assistant usage by model and all other usage into a separate bucket.
pub fn get_usage_cost_breakdown(entries: &[Value]) -> Vec<UsageCostBreakdownEntry> {
    let mut totals_by_key: BTreeMap<String, UsageTotals> = BTreeMap::new();
    for entry in entries {
        let entry_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (key, usage) = match entry_type {
            "message" => {
                let message = entry.get("message");
                match message
                    .and_then(|message| message.get("role"))
                    .and_then(Value::as_str)
                {
                    Some("assistant") => {
                        let provider = message
                            .and_then(|m| m.get("provider"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let model = message
                            .and_then(|m| m.get("responseModel"))
                            .and_then(Value::as_str)
                            .or_else(|| {
                                message.and_then(|m| m.get("model")).and_then(Value::as_str)
                            })
                            .unwrap_or_default();
                        (
                            Some(format!("{provider}/{model}")),
                            message.and_then(usage_from),
                        )
                    }
                    Some("toolResult") => (
                        Some("Tools/summaries".to_owned()),
                        message.and_then(usage_from),
                    ),
                    _ => (None, None),
                }
            }
            "usage" => (
                Some(format!(
                    "{}/{}",
                    entry["provider"].as_str().unwrap_or_default(),
                    entry["model"].as_str().unwrap_or_default()
                )),
                usage_from(entry),
            ),
            "custom"
                if entry["customType"] == "prompt-cache-prewarm"
                    && entry["data"]["phase"] == "warmed" =>
            {
                (Some("Tools/summaries".to_owned()), prewarm_usage(entry))
            }
            "branch_summary" | "compaction" => {
                (Some("Tools/summaries".to_owned()), usage_from(entry))
            }
            _ => (None, None),
        };
        let (Some(key), Some(usage)) = (key, usage) else {
            continue;
        };
        add_usage_to_totals(totals_by_key.entry(key).or_default(), &usage);
    }

    let mut breakdown: Vec<UsageCostBreakdownEntry> = totals_by_key
        .into_iter()
        .map(|(key, totals)| UsageCostBreakdownEntry {
            key,
            cost: totals.cost,
            cost_unknown: totals.cost_unknown,
            tokens: totals.input + totals.output + totals.cache_read + totals.cache_write,
        })
        .filter(|entry| entry.cost > 0.0 || entry.tokens > 0)
        .collect();
    breakdown.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    breakdown
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, total: f64) -> Value {
        json!({ "input": input, "output": output, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input + output,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": total } })
    }

    #[test]
    fn unknown_price_and_optional_splits_survive_combination() {
        let first = Usage {
            cost_unknown: Some(true),
            cache_write_1h: Some(4),
            reasoning: Some(2),
            ..Usage::default()
        };
        let second = Usage {
            cache_write_1h: Some(3),
            ..Usage::default()
        };
        let combined = combine_usage(first, second);
        assert_eq!(combined.cost_unknown, Some(true));
        assert_eq!(combined.cache_write_1h, Some(7));
        assert_eq!(combined.reasoning, Some(2));
        assert_eq!(
            combine_usage(Usage::default(), Usage::default()).reasoning,
            None
        );
        let mut totals = create_usage_totals();
        add_usage_to_totals(&mut totals, &combined);
        assert!(totals.cost_unknown);
    }
    #[test]
    fn prewarm_and_ledger_usage_are_attributed_and_unknown_cost_is_retained() {
        let mut unknown = usage(12, 0, 0.0);
        unknown["costUnknown"] = json!(true);
        let entries = vec![
            json!({"type":"custom","customType":"prompt-cache-prewarm","data":{"phase":"warmed","usage":unknown}}),
            json!({"type":"usage","provider":"p","model":"m","usage":usage(3,0,1.0)}),
            json!({"type":"custom","customType":"prompt-cache-prewarm","data":{"phase":"failed","usage":usage(100,0,10.0)}}),
        ];
        let result = get_usage_cost_breakdown(&entries);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].key, "p/m");
        assert_eq!(result[1].key, "Tools/summaries");
        assert!(result[1].cost_unknown);
        assert_eq!(result[1].tokens, 12);
    }

    #[test]
    fn a_new_totals_is_zeroed() {
        assert_eq!(create_usage_totals(), UsageTotals::default());
    }

    #[test]
    fn adding_usage_accumulates_every_field() {
        let mut totals = create_usage_totals();
        let first: Usage = serde_json::from_value(usage(10, 5, 0.5)).expect("usage");
        let second: Usage = serde_json::from_value(usage(1, 2, 0.25)).expect("usage");
        add_usage_to_totals(&mut totals, &first);
        add_usage_to_totals(&mut totals, &second);
        assert_eq!(totals.input, 11);
        assert_eq!(totals.output, 7);
        assert!((totals.cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn the_breakdown_groups_assistant_and_tool_usage() {
        let entries = vec![
            json!({ "type": "message", "message": { "role": "assistant", "provider": "anthropic", "model": "claude", "usage": usage(100, 10, 1.0) } }),
            json!({ "type": "message", "message": { "role": "toolResult", "usage": usage(5, 0, 0.1) } }),
            json!({ "type": "message", "message": { "role": "user" } }),
        ];
        let breakdown = get_usage_cost_breakdown(&entries);
        assert_eq!(breakdown.len(), 2);
        assert_eq!(breakdown[0].key, "anthropic/claude");
        assert_eq!(breakdown[0].tokens, 110);
        assert_eq!(breakdown[1].key, "Tools/summaries");
    }

    #[test]
    fn zero_usage_entries_are_dropped() {
        let entries = vec![
            json!({ "type": "message", "message": { "role": "assistant", "provider": "p", "model": "m", "usage": usage(0, 0, 0.0) } }),
        ];
        assert!(get_usage_cost_breakdown(&entries).is_empty());
    }
}
