use crate::{quota_command::QUOTA_MISSING_KEY, quota_types::QuotaSnapshot};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;
use url::form_urlencoded::Serializer;

const QUOTA_TIMEOUT_MS: u64 = 15_000;

fn field<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn money(amount: f64) -> String {
    format!("${amount:.2}")
}

fn window_limits(value: &Value) -> Vec<(String, f64, f64, Option<String>)> {
    value
        .get("windowLimits")
        .and_then(Value::as_array)
        .map(|limits| {
            limits
                .iter()
                .filter_map(|limit| {
                    let window = string(limit.get("window"))?;
                    let used = number(limit.get("used"))?;
                    let cap = number(limit.get("cap"))?;
                    let reset = limit
                        .get("resetAt")
                        .and_then(Value::as_f64)
                        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
                        .map(|instant| instant.format("%Y-%m-%d %H:%M UTC").to_string());
                    Some((window, used, cap, reset))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_credits(value: &Value) -> Option<String> {
    let credits = value.get("credits").filter(|inner| inner.is_object())?;
    let monthly = number(credits.get("monthlyCredits"));
    let purchased = number(credits.get("purchasedCredits"));
    let free = number(credits.get("freeCredits"));
    if monthly.is_none() && purchased.is_none() && free.is_none() {
        return None;
    }
    let remaining = monthly.unwrap_or(0.0) + purchased.unwrap_or(0.0) + free.unwrap_or(0.0);
    let mut lines = vec![format!("  Remaining: {}", money(remaining))];
    for (window, used, cap, reset) in window_limits(value) {
        let label = match window.as_str() {
            "fiveHour" => "5h",
            "weekly" => "Weekly",
            other => other,
        };
        let suffix = reset
            .map(|instant| format!(" (resets {instant})"))
            .unwrap_or_default();
        lines.push(format!(
            "  {label}: {} of {}{suffix}",
            money(used),
            money(cap)
        ));
    }
    Some(lines.join("\n"))
}

fn parse_subscription(value: &Value) -> Option<String> {
    let plan = string(field(value, &["data", "planId"]))
        .map(|plan| plan.replace(['_', '-'], " "))
        .unwrap_or_else(|| "none".to_owned());
    let status = string(field(value, &["data", "status"]));
    let period_end = string(field(value, &["data", "currentPeriodEnd"]));
    let mut line = format!("  {plan}");
    if let Some(status) = status {
        line.push_str(&format!(" ({status})"));
    }
    if let Some(period_end) = period_end {
        line.push_str(&format!(" — renews {period_end}"));
    }
    Some(line)
}

fn parse_usage(value: &Value) -> Option<String> {
    let total_cost = number(value.get("totalCost"))?;
    let total_count = number(value.get("totalCount"))?;
    let mut line = format!(
        "  Cost: {} across {} requests",
        money(total_cost),
        total_count as u64
    );
    if let Some(tokens) = number(value.get("totalTokens")).or_else(|| number(value.get("tokens"))) {
        line.push_str(&format!(" ({tokens} tokens)"));
    }
    Some(line)
}

pub async fn fetch_quota(api_base: &str, api_key: Option<&str>) -> QuotaSnapshot {
    let Some(key) = api_key.filter(|key| !key.is_empty()) else {
        return QuotaSnapshot {
            warning: Some(QUOTA_MISSING_KEY.to_owned()),
            ..Default::default()
        };
    };
    let client = reqwest::Client::new();
    let base = api_base.trim_end_matches('/');
    let request = async {
        let get = |url: String| {
            let client = client.clone();
            let key = key.to_owned();
            async move { client.get(url).bearer_auth(key).send().await }
        };
        let who = get(format!("{base}/alpha/whoami"))
            .await
            .map_err(|error| format!("Failed to fetch Command Code quota: {error}"))?;
        if matches!(who.status().as_u16(), 401 | 403) {
            return Err("Command Code rejected the API key".to_owned());
        }
        let who: Value = who
            .json()
            .await
            .map_err(|error| format!("Failed to fetch Command Code quota: {error}"))?;
        let login = string(field(&who, &["org", "login"]))
            .or_else(|| string(field(&who, &["user", "userName"])))
            .or_else(|| string(field(&who, &["user", "name"])));
        let org = string(field(&who, &["org", "id"]));
        let Some(login) = login else {
            return Err("Command Code returned an unrecognized account response".to_owned());
        };
        let query = |extra: &[(&str, &str)]| {
            let mut serializer = Serializer::new(String::new());
            if let Some(org) = org.as_deref() {
                serializer.append_pair("orgId", org);
            }
            for (name, value) in extra {
                serializer.append_pair(name, value);
            }
            let query = serializer.finish();
            if query.is_empty() {
                String::new()
            } else {
                format!("?{query}")
            }
        };
        let credits_url = format!("{base}/alpha/billing/credits{}", query(&[]));
        let subscriptions_url = format!("{base}/alpha/billing/subscriptions{}", query(&[]));
        let (credits, subscriptions) = tokio::join!(get(credits_url), get(subscriptions_url));

        let mut unavailable = Vec::new();
        let credits = match credits {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(value) => parse_credits(&value),
                    Err(_) => None,
                }
            }
            Ok(response) => {
                if matches!(response.status().as_u16(), 401 | 403) {
                    return Err("Command Code rejected the API key".to_owned());
                }
                None
            }
            Err(_) => None,
        };
        if credits.is_none() {
            unavailable.push("credits");
        }
        let subscriptions = match subscriptions {
            Ok(response) if response.status().is_success() => response
                .json::<Value>()
                .await
                .ok()
                .filter(|value| value.get("data").is_some()),
            Ok(response) => {
                if matches!(response.status().as_u16(), 401 | 403) {
                    return Err("Command Code rejected the API key".to_owned());
                }
                None
            }
            Err(_) => None,
        };
        if subscriptions.is_none() {
            unavailable.push("subscription");
        }
        let plan = subscriptions.as_ref().and_then(parse_subscription);
        let period_start = subscriptions
            .as_ref()
            .and_then(|value| string(field(value, &["data", "currentPeriodStart"])));
        let since: Vec<(&str, &str)> = period_start
            .as_deref()
            .map(|start| vec![("since", start)])
            .unwrap_or_default();
        let usage = match get(format!("{base}/alpha/usage/summary{}", query(&since))).await {
            Ok(response) if response.status().is_success() => response
                .json::<Value>()
                .await
                .ok()
                .and_then(|value| parse_usage(&value)),
            _ => None,
        };
        if usage.is_none() {
            unavailable.push("usage");
        }
        let account = Some(format!(
            "  {}",
            string(field(&who, &["user", "keyName"]))
                .or_else(|| string(field(&who, &["user", "displayName"])))
                .unwrap_or(login)
        ));
        let warning = if credits.is_none() && plan.is_none() && usage.is_none() {
            Some("Command Code quota details unavailable".to_owned())
        } else {
            None
        };
        Ok(QuotaSnapshot {
            credits,
            plan,
            usage,
            account,
            warning,
        })
    };
    match timeout(Duration::from_millis(QUOTA_TIMEOUT_MS), request).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => QuotaSnapshot {
            warning: Some(error),
            ..Default::default()
        },
        Err(_) => QuotaSnapshot {
            warning: Some("Command Code quota request timed out".to_owned()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credits_sum_all_buckets_and_include_window_limits() {
        let rendered = parse_credits(&json!({
            "credits": {"monthlyCredits": 5, "purchasedCredits": 2.5, "freeCredits": 1},
            "windowLimits": [{"window": "weekly", "used": 1.5, "cap": 10, "resetAt": null}]
        }))
        .expect("credits");
        assert!(rendered.contains("Remaining: $8.50"));
        assert!(rendered.contains("Weekly: $1.50 of $10.00"));
    }

    #[test]
    fn credits_need_at_least_one_bucket() {
        assert_eq!(parse_credits(&json!({"credits": {}})), None);
        assert_eq!(parse_credits(&json!({})), None);
    }

    #[test]
    fn a_subscription_renders_its_plan() {
        let rendered = parse_subscription(&json!({
            "data": {"planId": "pro_tier", "status": "active", "currentPeriodEnd": "2026-12-01"}
        }))
        .expect("subscription");
        assert!(rendered.contains("pro tier"));
        assert!(rendered.contains("active"));
        assert!(rendered.contains("2026-12-01"));
    }

    #[test]
    fn usage_needs_cost_and_count() {
        assert!(
            parse_usage(&json!({"totalCost": 1.25, "totalCount": 3, "totalTokens": 900}))
                .expect("usage")
                .contains("$1.25")
        );
        assert_eq!(parse_usage(&json!({"totalCost": 1.25})), None);
    }

    #[tokio::test]
    async fn a_missing_key_reports_the_login_hint() {
        let snapshot = fetch_quota("https://example.invalid", None).await;
        assert_eq!(snapshot.warning.as_deref(), Some(QUOTA_MISSING_KEY));
    }
}
