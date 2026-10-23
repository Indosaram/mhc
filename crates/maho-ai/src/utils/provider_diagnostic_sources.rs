//! Port of the reset-header part of senpi packages/ai/src/utils/provider-diagnostic-sources.ts
//! (`anthropicProviderDiagnosticFromError`). The Rust SDK error carries no JS `Error`, so the
//! caller classifies the failure first and passes the response headers through a lookup.

use crate::types::{ProviderDiagnostic, ProviderDiagnosticCategory};

fn header_number(headers: &dyn Fn(&str) -> Option<String>, name: &str) -> f64 {
    // JS `Number(null)` is 0 and `Number("")` is 0; an unparsable value is NaN.
    match headers(name) {
        None => 0.0,
        Some(value) if value.trim().is_empty() => 0.0,
        Some(value) => value.trim().parse::<f64>().unwrap_or(f64::NAN),
    }
}

/// For a `quota`/`rate_limit` Anthropic diagnostic, attaches `resets_at` from Claude's unified
/// rate-limit headers: windows at or past 100% utilization whose reset is still in the future,
/// the latest of them winning.
pub fn with_anthropic_unified_reset(
    diagnostic: Option<ProviderDiagnostic>,
    headers: &dyn Fn(&str) -> Option<String>,
    now_ms: i64,
) -> Option<ProviderDiagnostic> {
    let mut diagnostic = diagnostic?;
    if !matches!(
        diagnostic.category,
        ProviderDiagnosticCategory::Quota | ProviderDiagnosticCategory::RateLimit
    ) {
        return Some(diagnostic);
    }
    let latest = ["5h", "7d"]
        .iter()
        .filter_map(|window| {
            let utilization = header_number(
                headers,
                &format!("anthropic-ratelimit-unified-{window}-utilization"),
            );
            let reset = header_number(
                headers,
                &format!("anthropic-ratelimit-unified-{window}-reset"),
            ) * 1000.0;
            (utilization >= 1.0 && reset.is_finite() && reset > now_ms as f64)
                .then_some(reset as i64)
        })
        .max();
    if let Some(reset) = latest {
        diagnostic.resets_at = Some(reset);
    }
    Some(diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderDiagnosticEvidence;

    fn quota() -> ProviderDiagnostic {
        ProviderDiagnostic {
            category: ProviderDiagnosticCategory::Quota,
            http_status: Some(429),
            code: None,
            evidence: ProviderDiagnosticEvidence::StructuredStatus,
            resets_at: None,
        }
    }

    #[test]
    fn latest_exhausted_future_window_wins() {
        let headers = |name: &str| -> Option<String> {
            match name {
                "anthropic-ratelimit-unified-5h-utilization" => Some("1".into()),
                "anthropic-ratelimit-unified-5h-reset" => Some("2000000000".into()),
                "anthropic-ratelimit-unified-7d-utilization" => Some("0.4".into()),
                "anthropic-ratelimit-unified-7d-reset" => Some("2100000000".into()),
                _ => None,
            }
        };
        let out = with_anthropic_unified_reset(Some(quota()), &headers, 1_000).expect("diagnostic");
        assert_eq!(out.resets_at, Some(2_000_000_000_000));
    }
}
