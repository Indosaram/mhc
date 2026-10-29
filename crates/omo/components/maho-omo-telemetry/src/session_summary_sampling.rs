//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-sampling.ts`.

use sha2::{Digest, Sha256};

pub const RAW_TURN_SAMPLE_RATE: f64 = 0.05;
pub const RAW_TURN_SAMPLE_RATE_ENV: &str = "OMO_TELEMETRY_RAW_TURN_SAMPLE_RATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySink {
    Posthog,
    Collector,
}

pub fn is_session_sampled(session_hash: &str, sample_rate: f64) -> bool {
    if sample_rate >= 1.0 {
        return true;
    }
    if sample_rate <= 0.0 {
        return false;
    }
    let digest = Sha256::digest(format!("turn-sample:{session_hash}").as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    f64::from(bucket) / 4_294_967_296.0 < sample_rate
}

/// Share of sessions that also send every raw `turn_completed`. The collector stores and samples
/// server-side, so its sink always gets every turn; on the direct PostHog path the env override wins over
/// `telemetry.rawTurnSampleRate`, which wins over the default. Values outside [0, 1] are ignored.
pub fn resolve_raw_turn_sample_rate(
    sink: TelemetrySink,
    configured: Option<f64>,
    env: &telemetry_core::TelemetryEnv,
) -> f64 {
    if sink == TelemetrySink::Collector {
        return 1.0;
    }
    parse_rate(env.get(RAW_TURN_SAMPLE_RATE_ENV).map(String::as_str))
        .or_else(|| valid_rate(configured))
        .unwrap_or(RAW_TURN_SAMPLE_RATE)
}

fn parse_rate(value: Option<&str>) -> Option<f64> {
    let trimmed = value.unwrap_or_default().trim();
    if trimmed.is_empty() {
        return None;
    }
    valid_rate(trimmed.parse().ok())
}

fn valid_rate(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> telemetry_core::TelemetryEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn sampling_is_stable_and_bounded() {
        let hash = "a".repeat(64);
        assert_eq!(
            is_session_sampled(&hash, 0.05),
            is_session_sampled(&hash, 0.05)
        );
        assert!(is_session_sampled(&hash, 1.0));
        assert!(!is_session_sampled(&hash, 0.0));
        assert!(!is_session_sampled(&hash, -1.0));
        assert!(is_session_sampled(&hash, 2.0));
        let sampled = (0..1000)
            .filter(|index| is_session_sampled(&format!("session-{index}"), 0.05))
            .count();
        assert!((10..90).contains(&sampled), "sampled {sampled}");
    }

    #[test]
    fn precedence_is_env_then_config_then_default() {
        assert_eq!(
            resolve_raw_turn_sample_rate(TelemetrySink::Collector, Some(0.2), &env(&[])),
            1.0
        );
        assert_eq!(
            resolve_raw_turn_sample_rate(
                TelemetrySink::Posthog,
                Some(0.2),
                &env(&[("OMO_TELEMETRY_RAW_TURN_SAMPLE_RATE", "0.7")])
            ),
            0.7
        );
        assert_eq!(
            resolve_raw_turn_sample_rate(TelemetrySink::Posthog, Some(0.2), &env(&[])),
            0.2
        );
        assert_eq!(
            resolve_raw_turn_sample_rate(TelemetrySink::Posthog, None, &env(&[])),
            RAW_TURN_SAMPLE_RATE
        );
    }

    #[test]
    fn out_of_range_values_are_ignored() {
        for value in ["", "  ", "-0.5", "1.5", "nan", "abc"] {
            assert_eq!(
                resolve_raw_turn_sample_rate(
                    TelemetrySink::Posthog,
                    Some(0.3),
                    &env(&[("OMO_TELEMETRY_RAW_TURN_SAMPLE_RATE", value)])
                ),
                0.3,
                "{value:?}"
            );
        }
        assert_eq!(
            resolve_raw_turn_sample_rate(TelemetrySink::Posthog, Some(2.0), &env(&[])),
            RAW_TURN_SAMPLE_RATE
        );
        assert_eq!(
            resolve_raw_turn_sample_rate(TelemetrySink::Posthog, Some(1.0), &env(&[])),
            1.0
        );
    }
}
