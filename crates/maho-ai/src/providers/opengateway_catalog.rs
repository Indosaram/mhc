//! Parsing helpers for the public OpenGateway model price catalog.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct PriceRoute {
    pub route: String,
    pub model_id: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

fn is_price_route_entry(value: &Value) -> bool {
    value.as_object().is_some_and(|entry| {
        entry.get("provider").and_then(Value::as_str).is_some()
            && entry.get("modelOwner").and_then(Value::as_str).is_some()
            && entry.get("modelName").and_then(Value::as_str).is_some()
    })
}

fn per_million(value: Option<&Value>) -> Option<f64> {
    let value = value?.as_f64()?;
    (value.is_finite() && value >= 0.0).then_some(value * 1_000_000.0)
}

/// Parse either the current metadata envelope or the older flat route table.
pub fn parse_open_gateway_price_table(value: &Value) -> Result<Vec<PriceRoute>, String> {
    let object = value.as_object().ok_or_else(|| "OpenGateway price table is not an object".to_owned())?;
    let envelope = object.get("entries").is_some_and(|entries| !is_price_route_entry(entries));
    if envelope {
        if let Some(currency) = object.get("currency")
            && currency.as_str() != Some("USD") {
                return Err(format!("OpenGateway price table has unsupported currency: {} (expected USD)", currency));
            }
        if let Some(unit) = object.get("unit")
            && unit.as_str() != Some("USD_PER_TOKEN") {
                return Err(format!("OpenGateway price table has unsupported unit: {} (expected USD_PER_TOKEN)", unit));
            }
    }
    let entries = if envelope { object.get("entries").expect("validated catalog envelope has entries") } else { value };
    let entries = entries.as_object().ok_or_else(|| "OpenGateway price table entries is not an object".to_owned())?;
    let mut routes = Vec::new();
    for entry in entries.values() {
        let Some(row) = entry.as_object() else { continue };
        let (Some(provider), Some(owner), Some(model)) = (
            row.get("provider").and_then(Value::as_str),
            row.get("modelOwner").and_then(Value::as_str),
            row.get("modelName").and_then(Value::as_str),
        ) else { continue };
        let effective = row.get("pricing").and_then(Value::as_object)
            .and_then(|p| p.get("current")).and_then(Value::as_object)
            .and_then(|c| c.get("effectivePrice")).and_then(Value::as_object);
        let price = effective.unwrap_or(row);
        routes.push(PriceRoute {
            route: provider.to_owned(), model_id: format!("{owner}/{model}"),
            input: per_million(price.get("inputCostPerToken")),
            output: per_million(price.get("outputCostPerToken")),
            cache_read: per_million(price.get("cacheReadInputTokenCost")),
            cache_write: per_million(price.get("cacheCreationInputTokenCost")),
        });
    }
    if routes.is_empty() { return Err("OpenGateway price table has no priced routes".into()); }
    Ok(routes)
}



#[allow(dead_code)]
fn _record_type_anchor(_: &Map<String, Value>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route() -> Value { json!({"provider":"route-a","modelOwner":"owner","modelName":"model","inputCostPerToken":0.000001}) }

    #[test]
    fn parses_envelope_and_legacy_reserved_route_keys() {
        let envelope = json!({"currency":"USD","unit":"USD_PER_TOKEN","entries":{"x":route()}});
        assert_eq!(parse_open_gateway_price_table(&envelope).unwrap()[0].route, "route-a");
        for key in ["entries", "currency", "unit"] {
            let flat = json!({key:route()});
            assert_eq!(parse_open_gateway_price_table(&flat).unwrap()[0].model_id, "owner/model");
        }
    }

    #[test]
    fn rejects_unsupported_envelope_metadata() {
        let entries = json!({"x":route()});
        assert!(parse_open_gateway_price_table(&json!({"entries":entries,"currency":"EUR"})).is_err());
        assert!(parse_open_gateway_price_table(&json!({"entries":entries,"unit":"OTHER"})).is_err());
    }
}
