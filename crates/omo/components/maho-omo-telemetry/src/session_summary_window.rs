use serde_json::{Map, Value, json};

const SUMMED: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "total_tokens",
    "cost_usd",
    "tool_result_count",
    "tool_error_count",
];
const COUNTED: &[&str] = &[
    "invalid_usage_turns",
    "error_turns",
    "aborted_turns",
    "custom_model_turns",
];
const PER_MODEL: &[&str] = &[
    "turns",
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "cost_usd",
];

pub fn open_window(at: f64, index: &Value) -> Value {
    json!({"startedAt":at,"lastAt":at,"firstTurnIndex":index,"lastTurnIndex":index,"sums":{},"totals":[],"models":[]})
}

fn add(sums: &mut Value, key: &str, amount: f64) {
    sums[key] = json!(sums[key].as_f64().unwrap_or(0.0) + amount);
}

pub fn add_turn(window: &mut Value, properties: &Value, event: &Value, invalid: bool, at: f64) {
    window["lastAt"] = json!(at);
    window["lastTurnIndex"] = properties["turn_index"].clone();
    if let Some(totals) = window["totals"].as_array_mut() {
        totals.push(properties["total_tokens"].clone());
    }
    let results = event["toolResults"].as_array();
    for key in SUMMED {
        let amount = match *key {
            "tool_result_count" => json!(results.map_or(0, Vec::len)).as_f64().unwrap_or(0.0),
            "tool_error_count" => {
                json!(results.map_or(0, |r| r.iter().filter(|r| r["isError"] == true).count()))
                    .as_f64()
                    .unwrap_or(0.0)
            }
            "cost_usd" => event["message"]["usage"]["cost"]["total"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .unwrap_or(0.0),
            _ => properties[key].as_f64().unwrap_or(0.0),
        };
        add(&mut window["sums"], key, amount);
    }
    for (key, applies) in [
        ("invalid_usage_turns", invalid),
        ("error_turns", event["message"]["stopReason"] == "error"),
        ("aborted_turns", event["message"]["stopReason"] == "aborted"),
        ("custom_model_turns", properties["model_id"] == "custom"),
    ] {
        if applies {
            add(&mut window["sums"], key, 1.0);
        }
    }
    if let Some(models) = window["models"].as_array_mut() {
        let index = models.iter().position(|m| {
            m["provider"] == properties["provider"] && m["model"] == properties["model_id"]
        });
        let model = match index {
            Some(index) => &mut models[index],
            None => {
                models.push(json!({"provider":properties["provider"],"model":properties["model_id"],"sums":{}}));
                let last = models.len() - 1;
                &mut models[last]
            }
        };
        for key in PER_MODEL {
            let amount = match *key {
                "turns" => 1.0,
                "cost_usd" => event["message"]["usage"]["cost"]["total"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .unwrap_or(0.0),
                _ => properties[key].as_f64().unwrap_or(0.0),
            };
            add(&mut model["sums"], key, amount);
        }
    }
}

fn cost(value: f64) -> f64 {
    ((value + f64::EPSILON) * 10_000.0).round() / 10_000.0
}

pub fn summary_properties(
    hash: &str,
    window: &Value,
    seq: u64,
    ended_by: &str,
    role: &str,
    rate: f64,
) -> Value {
    let mut totals: Vec<_> = window["totals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    totals.sort_by(f64::total_cmp);
    // Nearest-rank quantiles, expressed with integer arithmetic to avoid lossy index casts.
    let quantile = |numerator: usize, denominator: usize| {
        totals
            .get(
                totals
                    .len()
                    .saturating_mul(numerator)
                    .div_ceil(denominator)
                    .saturating_sub(1),
            )
            .copied()
            .unwrap_or(0.0)
    };
    let mut models: Vec<_> = window["models"].as_array().into_iter().flatten().collect();
    models.sort_by(|a, b| {
        b["sums"]["turns"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["sums"]["turns"].as_f64().unwrap_or(0.0))
            .then_with(|| {
                b["sums"]["total_tokens"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&a["sums"]["total_tokens"].as_f64().unwrap_or(0.0))
            })
    });
    let rest_sum = |key: &str| {
        models
            .iter()
            .skip(3)
            .map(|m| m["sums"][key].as_f64().unwrap_or(0.0))
            .sum::<f64>()
    };
    let mut props = json!({"$session_id":hash,"summary_seq":seq,"is_final":ended_by=="shutdown","ended_by":ended_by,"session_role":role,"sample_rate":rate,"raw_turns_sampled":is_session_sampled(hash,rate),"turns":totals.len(),"first_turn_index":window["firstTurnIndex"],"last_turn_index":window["lastTurnIndex"],"window_ms":window["lastAt"].as_f64().unwrap_or(0.0)-window["startedAt"].as_f64().unwrap_or(0.0),"total_tokens_p50":quantile(1,2),"total_tokens_p95":quantile(95,100),"total_tokens_max":totals.last().copied().unwrap_or(0.0),"model_count":models.len(),"other_turns":rest_sum("turns"),"other_total_tokens":rest_sum("total_tokens"),"other_cost_usd":cost(rest_sum("cost_usd"))});
    if let Some(props) = props.as_object_mut() {
        for key in SUMMED.iter().chain(COUNTED) {
            let value = window["sums"][key].as_f64().unwrap_or(0.0);
            props.insert(
                (*key).into(),
                json!(if *key == "cost_usd" {
                    cost(value)
                } else {
                    value
                }),
            );
        }
        for slot in 1..=3 {
            let model = models.get(slot - 1);
            props.insert(
                format!("m{slot}_provider"),
                model.map_or(json!("none"), |m| m["provider"].clone()),
            );
            props.insert(
                format!("m{slot}_model"),
                model.map_or(json!("none"), |m| m["model"].clone()),
            );
            for key in PER_MODEL {
                let value = model.and_then(|m| m["sums"][key].as_f64()).unwrap_or(0.0);
                props.insert(
                    format!("m{slot}_{key}"),
                    json!(if *key == "cost_usd" {
                        cost(value)
                    } else {
                        value
                    }),
                );
            }
        }
    }
    props
}

pub fn is_session_sampled(hash: &str, rate: f64) -> bool {
    use sha2::{Digest, Sha256};
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    let digest = Sha256::digest(format!("turn-sample:{hash}"));
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    f64::from(bucket) / 4_294_967_296.0 < rate
}

/// Disk data is untrusted. Rebuild only schema-declared, finite numeric fields and masked names.
pub fn parse_window(value: &Value) -> Option<Value> {
    let mut result = Map::new();
    for key in ["startedAt", "lastAt", "firstTurnIndex", "lastTurnIndex"] {
        result.insert(
            key.into(),
            json!(value[key].as_f64().filter(|n| n.is_finite() && *n >= 0.0)?),
        );
    }
    let sums = |raw: &Value, keys: &[&str]| -> Option<Value> {
        raw.as_object()?;
        let mut parsed = Map::new();
        for &key in keys {
            if let Some(v) = raw.get(key) {
                parsed.insert(
                    key.into(),
                    json!(v.as_f64().filter(|n| n.is_finite() && *n >= 0.0)?),
                );
            }
        }
        Some(Value::Object(parsed))
    };
    let keys: Vec<_> = SUMMED.iter().chain(COUNTED).copied().collect();
    result.insert("sums".into(), sums(&value["sums"], &keys)?);
    let totals: Option<Vec<_>> = value["totals"]
        .as_array()?
        .iter()
        .map(|n| n.as_f64().filter(|n| n.is_finite() && *n >= 0.0))
        .collect();
    let totals = totals?;
    if totals.len() > 50 {
        return None;
    }
    result.insert("totals".into(), json!(totals));
    let mut models = Vec::new();
    for model in value["models"].as_array()? {
        let (provider, id) = crate::product_identity::mask_provider_and_model(
            model["provider"].as_str()?,
            model["model"].as_str()?,
        );
        models.push(json!({"provider":provider,"model":id,"sums":sums(&model["sums"],PER_MODEL)?}));
    }
    if models.len() > 50 {
        return None;
    }
    result.insert("models".into(), json!(models));
    Some(Value::Object(result))
}
