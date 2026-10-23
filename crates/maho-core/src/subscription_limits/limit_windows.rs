//! Port of senpi packages/coding-agent/src/core/subscription-limits/limit-windows.ts.
//!
//! The limit windows a subscription reports with its responses, read into one shape: which window
//! (5-hour, weekly, ...), how much of it is used, and when it resets. A window nothing reported is
//! absent, never 0%.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

/// One reported limit window.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    /// Stable key within an account: `<limit>:<window>` (`codex:primary`, `claude:5h`).
    pub id: String,
    /// What the window is, as a person reads it: `5h`, `7d`, `7d opus`.
    pub label: String,
    pub window_minutes: Option<u64>,
    /// 0-100.
    pub used_percent: f64,
    /// ISO timestamp.
    pub resets_at: Option<String>,
}

fn number(value: Option<&String>) -> Option<f64> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|parsed| parsed.is_finite())
}

fn iso(epoch_seconds: Option<f64>) -> Option<String> {
    let millis = (epoch_seconds? * 1000.0).round();
    if !millis.is_finite() || millis.abs() > 8.64e15 {
        return None;
    }
    // Bounded above, so the cast is exact.
    let instant = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis as i64)?;
    Some(instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// `300` -> `5h`, `10080` -> `7d`, otherwise minutes.
pub fn window_label(minutes: Option<u64>, fallback: &str) -> String {
    match minutes {
        None | Some(0) => fallback.to_owned(),
        Some(minutes) if minutes % 1440 == 0 => format!("{}d", minutes / 1440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
    }
}

fn lower(headers: &HashMap<String, String>) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect()
}

fn minutes_u64(minutes: Option<f64>) -> Option<u64> {
    // Fractional or negative window lengths carry no usable label; JS would print them verbatim.
    minutes
        .filter(|value| *value >= 0.0 && value.fract() == 0.0 && *value < 9.0e15)
        .map(|value| value as u64)
}

fn codex_window(
    headers: &HashMap<String, String>,
    family: &str,
    slot: &str,
) -> Option<LimitWindow> {
    let used = number(headers.get(&format!("x-{family}-{slot}-used-percent")))?;
    let minutes = number(headers.get(&format!("x-{family}-{slot}-window-minutes")));
    let reset_at = number(headers.get(&format!("x-{family}-{slot}-reset-at")));
    // The same rule the Codex client applies: an all-zero window carries no information.
    if used == 0.0 && minutes.unwrap_or(0.0) == 0.0 && reset_at.is_none() {
        return None;
    }
    let limit = family.replace('-', "_");
    let window_minutes = minutes_u64(minutes);
    let label = if limit == "codex" {
        window_label(window_minutes, slot)
    } else {
        format!("{} {limit}", window_label(window_minutes, slot))
    };
    Some(LimitWindow {
        id: format!("{limit}:{slot}"),
        label,
        window_minutes,
        used_percent: used,
        resets_at: iso(reset_at),
    })
}

/// Every Codex limit family the response headers carry.
pub fn codex_windows_from_headers(raw: &HashMap<String, String>) -> Vec<LimitWindow> {
    let headers = lower(raw);
    let mut families = BTreeSet::new();
    for name in headers.keys() {
        let family = name
            .strip_prefix("x-")
            .and_then(|rest| {
                rest.strip_suffix("-primary-used-percent")
                    .or_else(|| rest.strip_suffix("-secondary-used-percent"))
            })
            .filter(|family| {
                !family.is_empty()
                    && family
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        if let Some(family) = family {
            families.insert(family.to_owned());
        }
    }
    families
        .iter()
        .flat_map(|family| {
            ["primary", "secondary"]
                .into_iter()
                .filter_map(|slot| codex_window(&headers, family, slot))
        })
        .collect()
}

const CLAUDE_WINDOWS: [(&str, u64); 2] = [("5h", 300), ("7d", 10080)];

/// Claude's unified rate-limit headers (utilization is a 0-1 fraction).
pub fn claude_windows_from_headers(raw: &HashMap<String, String>) -> Vec<LimitWindow> {
    let headers = lower(raw);
    CLAUDE_WINDOWS
        .iter()
        .filter_map(|(key, minutes)| {
            let utilization =
                number(headers.get(&format!("anthropic-ratelimit-unified-{key}-utilization")))?;
            let resets_at = iso(number(
                headers.get(&format!("anthropic-ratelimit-unified-{key}-reset")),
            ));
            Some(LimitWindow {
                id: format!("claude:{key}"),
                label: (*key).to_owned(),
                window_minutes: Some(*minutes),
                used_percent: (utilization * 10_000.0).round() / 100.0,
                resets_at,
            })
        })
        .collect()
}

fn sdk_window(kind: &str) -> Option<(&'static str, &'static str, u64)> {
    match kind {
        "five_hour" => Some(("claude:5h", "5h", 300)),
        "seven_day" => Some(("claude:7d", "7d", 10080)),
        "seven_day_opus" => Some(("claude:7d_opus", "7d opus", 10080)),
        "seven_day_sonnet" => Some(("claude:7d_sonnet", "7d sonnet", 10080)),
        _ => None,
    }
}

/// The Claude SDK's `rate_limit_event` (`rate_limit_info`: type, 0-1 utilization, reset epoch seconds).
pub fn claude_windows_from_sdk_event(data: &Value) -> Vec<LimitWindow> {
    if data.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
        return Vec::new();
    }
    let info = data.get("rate_limit_info");
    let window = info
        .and_then(|info| info.get("rateLimitType"))
        .and_then(Value::as_str)
        .and_then(sdk_window);
    let utilization = info
        .and_then(|info| info.get("utilization"))
        .and_then(Value::as_f64);
    let (Some((id, label, minutes)), Some(utilization)) = (window, utilization) else {
        return Vec::new();
    };
    let resets_at = iso(info
        .and_then(|info| info.get("resetsAt"))
        .and_then(Value::as_f64));
    vec![LimitWindow {
        id: id.to_owned(),
        label: label.to_owned(),
        window_minutes: Some(minutes),
        used_percent: (utilization * 10_000.0).round() / 100.0,
        resets_at,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn labels_follow_the_largest_whole_unit() {
        assert_eq!(window_label(Some(300), "x"), "5h");
        assert_eq!(window_label(Some(10080), "x"), "7d");
        assert_eq!(window_label(Some(45), "x"), "45m");
        assert_eq!(window_label(None, "primary"), "primary");
        assert_eq!(window_label(Some(0), "primary"), "primary");
    }

    #[test]
    fn codex_headers_become_windows_and_all_zero_windows_vanish() {
        let raw = headers(&[
            ("X-Codex-Primary-Used-Percent", "42"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1700000000"),
            ("x-codex-secondary-used-percent", "0"),
        ]);
        let windows = codex_windows_from_headers(&raw);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "codex:primary");
        assert_eq!(windows[0].label, "5h");
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("2023-11-14T22:13:20.000Z")
        );
    }

    #[test]
    fn claude_headers_scale_the_fraction() {
        let raw = headers(&[("anthropic-ratelimit-unified-5h-utilization", "0.256")]);
        let windows = claude_windows_from_headers(&raw);
        assert_eq!(windows.len(), 1);
        assert!((windows[0].used_percent - 25.6).abs() < 1e-9);
        assert_eq!(windows[0].resets_at, None);
    }

    #[test]
    fn sdk_event_maps_known_window_types_only() {
        let event = serde_json::json!({"type": "rate_limit_event", "rate_limit_info": {"rateLimitType": "seven_day_opus", "utilization": 0.5}});
        assert_eq!(claude_windows_from_sdk_event(&event)[0].label, "7d opus");
        let unknown = serde_json::json!({"type": "rate_limit_event", "rate_limit_info": {"rateLimitType": "other", "utilization": 0.5}});
        assert!(claude_windows_from_sdk_event(&unknown).is_empty());
    }
}
