//! Port of handoff-hint/estimate.ts.
use maho_ai::model::Model;
use maho_ai::models::{calculate_cost_at, model_cost::cost_card_at};
use maho_ai::types::Usage;

#[derive(Clone, Debug, PartialEq)]
pub struct HandoffEstimate {
    pub context_tokens: u64,
    pub fresh_tokens: u64,
    pub next_reply_usd: f64,
    pub fresh_usd: f64,
    pub saving_usd: f64,
}

/// Price of sending `tokens` of prompt with no warm cache: a cache write where the model prices one,
/// else plain input. Uses the model's own cost card, tiers included.
fn cold_prompt_usd(model: &Model, tokens: u64, at_ms: i64) -> f64 {
    let writes = cost_card_at(&model.cost, at_ms).cache_write > 0.0;
    let mut usage = Usage { context_usage: None,
        input: if writes { 0 } else { tokens },
        cache_write: if writes { tokens } else { 0 },
        total_tokens: tokens,
        ..Usage::default()
    };
    calculate_cost_at(model, &mut usage, at_ms).total
}

pub fn estimate_handoff(
    model: &Model,
    context_tokens: u64,
    fresh_tokens: u64,
    at_ms: i64,
) -> Option<HandoffEstimate> {
    let next_reply_usd = cold_prompt_usd(model, context_tokens, at_ms);
    if next_reply_usd.is_nan() || next_reply_usd <= 0.0 {
        return None;
    }
    let fresh = fresh_tokens.min(context_tokens);
    let fresh_usd = cold_prompt_usd(model, fresh, at_ms);
    Some(HandoffEstimate {
        context_tokens,
        fresh_tokens: fresh,
        next_reply_usd,
        fresh_usd,
        saving_usd: next_reply_usd - fresh_usd,
    })
}

pub fn format_usd(value: f64) -> String {
    if value < 0.01 {
        "under $0.01".into()
    } else {
        format!("${value:.2}")
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else {
        format!("{}k", (tokens as f64 / 1000.0).round())
    }
}

pub fn hint_text(estimate: &HandoffEstimate, subscription: bool) -> String {
    // The numbers lead: a one-line banner that clips its tail must still show them.
    let prices = if subscription { " at API prices" } else { "" };
    let plan = if subscription {
        " On your plan it counts toward your usage limits."
    } else {
        ""
    };
    format!(
        "The next reply would cost about {}{prices}; a fresh session about {}. This session is {} tokens and its cache has expired.{plan}",
        format_usd(estimate.next_reply_usd),
        format_usd(estimate.fresh_usd),
        format_tokens(estimate.context_tokens),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(cache_write: f64) -> Model {
        serde_json::from_value(json!({
            "id": "m", "name": "m", "api": "test", "provider": "test",
            "baseUrl": "https://example.invalid", "reasoning": false, "input": ["text"],
            "contextWindow": 1_000_000, "maxTokens": 1000,
            "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": cache_write}
        }))
        .expect("valid model fixture")
    }

    #[test]
    fn uses_cache_write_price_when_the_model_prices_one() {
        let estimate = estimate_handoff(&model(10.0), 500_000, 100_000, 0).expect("estimate");
        assert!((estimate.next_reply_usd - 5.0).abs() < 1e-9);
        assert!((estimate.fresh_usd - 1.0).abs() < 1e-9);
        assert!((estimate.saving_usd - 4.0).abs() < 1e-9);
    }

    #[test]
    fn falls_back_to_input_price_and_clamps_fresh_tokens() {
        let estimate = estimate_handoff(&model(0.0), 100_000, 900_000, 0).expect("estimate");
        assert_eq!(estimate.fresh_tokens, 100_000);
        assert!((estimate.next_reply_usd - 0.3).abs() < 1e-9);
        assert!(estimate.saving_usd.abs() < 1e-9);
    }

    #[test]
    fn free_models_have_no_estimate() {
        let mut free = model(0.0);
        free.cost.input = 0.0;
        assert!(estimate_handoff(&free, 100_000, 1, 0).is_none());
    }

    #[test]
    fn formats_money_and_tokens() {
        assert_eq!(format_usd(0.004), "under $0.01");
        assert_eq!(format_usd(1.234), "$1.23");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(250_400), "250k");
    }

    #[test]
    fn hint_text_leads_with_prices() {
        let estimate = HandoffEstimate {
            context_tokens: 300_000,
            fresh_tokens: 20_000,
            next_reply_usd: 2.0,
            fresh_usd: 0.1,
            saving_usd: 1.9,
        };
        assert_eq!(
            hint_text(&estimate, false),
            "The next reply would cost about $2.00; a fresh session about $0.10. This session is 300k tokens and its cache has expired."
        );
        let plan = hint_text(&estimate, true);
        assert!(plan.starts_with("The next reply would cost about $2.00 at API prices;"));
        assert!(plan.ends_with(" On your plan it counts toward your usage limits."));
    }
}
