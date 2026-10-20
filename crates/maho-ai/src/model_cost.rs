//! Port of senpi packages/ai/src/model-cost.ts.

use crate::types::{ModelCost, ModelCostCard};

/// The card in force at `at_ms`: the latest started dated card, else the base card.
pub fn cost_card_at(cost: &ModelCost, at_ms: i64) -> ModelCostCard {
    let mut selected = None;
    let mut started_at = i64::MIN;
    for rate in cost.effective_rates.iter().flatten() {
        // Like Date.parse's NaN, an invalid date never matches a request time.
        let Ok(from) = chrono::DateTime::parse_from_rfc3339(&rate.effective_from) else {
            continue;
        };
        let from = from.timestamp_millis();
        if from <= at_ms && from > started_at {
            selected = Some(&rate.card);
            started_at = from;
        }
    }
    selected.cloned().unwrap_or_else(|| ModelCostCard {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        tiers: cost.tiers.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::calculate_cost_at;
    use crate::types::{Model, Usage, UsageCost};
    use serde_json::json;

    const START: i64 = 1_796_112_000_000;

    fn model() -> Model {
        serde_json::from_value(json!({
            "id": "dated-price-test", "name": "Dated price test",
            "api": "test", "provider": "test", "baseUrl": "https://example.invalid",
            "reasoning": false, "input": ["text"],
            "contextWindow": 1_000_000, "maxTokens": 1000,
            "cost": {
                "input": 2, "output": 10, "cacheRead": 0.5, "cacheWrite": 1,
                "effectiveRates": [{
                    "effectiveFrom": "2026-12-01T08:00:00Z",
                    "input": 4, "output": 20, "cacheRead": 1, "cacheWrite": 2
                }]
            }
        }))
        .expect("valid model fixture")
    }

    fn million_each() -> Usage {
        Usage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
            ..Usage::default()
        }
    }

    #[test]
    fn bills_base_card_before_effective_instant() {
        let model = model();
        let mut usage = million_each();
        let cost = calculate_cost_at(&model, &mut usage, START - 1);
        assert_eq!(
            cost,
            UsageCost {
                input: 2.0,
                output: 10.0,
                cache_read: 0.5,
                cache_write: 1.0,
                total: 13.5,
            }
        );
        assert_eq!(usage.cost, cost);
    }

    #[test]
    fn bills_scheduled_card_at_effective_instant() {
        let model = model();
        let mut usage = million_each();
        let cost = calculate_cost_at(&model, &mut usage, START);
        assert_eq!(
            cost,
            UsageCost {
                input: 4.0,
                output: 20.0,
                cache_read: 1.0,
                cache_write: 2.0,
                total: 27.0,
            }
        );
        assert_eq!(usage.cost, cost);
    }

    #[test]
    fn selects_latest_started_card_in_unsorted_schedule() {
        let cost: ModelCost = serde_json::from_value(json!({
            "input": 1, "output": 0, "cacheRead": 0, "cacheWrite": 0,
            "effectiveRates": [
                {"effectiveFrom": "2027-06-01T00:00:00Z", "input": 8, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                {"effectiveFrom": "2026-12-01T08:00:00.000Z", "input": 4, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                {"effectiveFrom": "invalid", "input": 99, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                {"effectiveFrom": "2026-12-01T08:00:00Z", "input": 6, "output": 0, "cacheRead": 0, "cacheWrite": 0}
            ]
        })).expect("valid cost fixture");
        for (at_ms, expected) in [(START - 1, 1.0), (START, 4.0), (1_811_808_000_000, 8.0)] {
            assert_eq!(cost_card_at(&cost, at_ms).input, expected);
        }
    }

    #[test]
    fn scheduled_card_replaces_base_tiers_and_prices_one_hour_writes() {
        let mut model = model();
        model.cost.tiers = Some(vec![crate::types::ModelCostTier {
            input: 99.0,
            input_tokens_above: 0,
            ..Default::default()
        }]);
        let mut usage = million_each();
        usage.cache_write_1h = Some(500_000);
        let cost = calculate_cost_at(&model, &mut usage, START);
        assert_eq!(cost.input, 4.0);
        assert_eq!(cost.cache_write, 5.0);
    }

    #[test]
    fn scheduled_tiers_use_highest_strictly_exceeded_total_input_threshold() {
        let mut model = model();
        model.cost.effective_rates = Some(serde_json::from_value(json!([{
            "effectiveFrom": "2026-12-01T08:00:00Z",
            "input": 4, "output": 20, "cacheRead": 1, "cacheWrite": 2,
            "tiers": [
                {"inputTokensAbove": 3_000_000, "input": 16, "output": 80, "cacheRead": 4, "cacheWrite": 8},
                {"inputTokensAbove": 2_000_000, "input": 8, "output": 40, "cacheRead": 2, "cacheWrite": 4}
            ]
        }])).expect("valid scheduled tiers"));
        for (extra, expected) in [(0, 40.0), (1, 80.0)] {
            let mut usage = million_each();
            usage.cache_read += extra;
            let cost = calculate_cost_at(&model, &mut usage, START);
            assert_eq!(cost.output, expected);
        }
    }

    #[test]
    fn scheduled_card_survives_json_round_trip() {
        let cost = model().cost;
        let value = serde_json::to_value(&cost).expect("serialize cost");
        assert_eq!(
            value["effectiveRates"][0]["effectiveFrom"],
            "2026-12-01T08:00:00Z"
        );
        assert_eq!(value["effectiveRates"][0]["input"], 4);
        assert_eq!(
            serde_json::from_value::<ModelCost>(value).expect("deserialize cost"),
            cost
        );
    }
}
