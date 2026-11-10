use maho_ai::types::{ModelCost, Usage};

pub fn calculate_cost(cost: &ModelCost, usage: &mut Usage) {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates = cost.clone();
    let mut matched: Option<u64> = None;
    for tier in cost.tiers.iter().flatten() {
        if input_tokens > tier.input_tokens_above
            && matched.is_none_or(|current| tier.input_tokens_above > current)
        {
            matched = Some(tier.input_tokens_above);
            rates.input = tier.input;
            rates.output = tier.output;
            rates.cache_read = tier.cache_read;
            rates.cache_write = tier.cache_write;
        }
    }
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;
    usage.cost.input = rates.input / 1_000_000.0 * usage.input as f64;
    usage.cost.output = rates.output / 1_000_000.0 * usage.output as f64;
    usage.cost.cache_read = rates.cache_read / 1_000_000.0 * usage.cache_read as f64;
    usage.cost.cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_highest_applicable_tier() {
        let cost = ModelCost {
            input: 1.0,
            output: 1.0,
            cache_read: 1.0,
            cache_write: 1.0,
            tiers: Some(vec![
                maho_ai::types::ModelCostTier {
                    input_tokens_above: 10,
                    input: 2.0,
                    output: 2.0,
                    cache_read: 2.0,
                    cache_write: 2.0,
                },
                maho_ai::types::ModelCostTier {
                    input_tokens_above: 100,
                    input: 3.0,
                    output: 3.0,
                    cache_read: 3.0,
                    cache_write: 3.0,
                },
            ]),
            ..Default::default()
        };
        let mut usage = Usage {
            input: 101,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            ..Default::default()
        };
        calculate_cost(&cost, &mut usage);
        assert_eq!(usage.cost.input, 303.0 / 1_000_000.0);
    }

    #[test]
    fn one_hour_write_costs_twice_input_rate() {
        let cost = ModelCost {
            input: 5.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 1.0,
            ..Default::default()
        };
        let mut usage = Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 10,
            cache_write_1h: Some(10),
            ..Default::default()
        };
        calculate_cost(&cost, &mut usage);
        assert_eq!(usage.cost.total, 100.0 / 1_000_000.0);
    }
}
