pub use crate::types::{EffectiveInterval, RequestedInterval};
use crate::types::{EffectiveIntervalUnit, RequestedIntervalUnit};
pub struct NormalizedInterval { pub effective: EffectiveInterval, pub interval_ms: f64 }
pub fn normalize_interval(requested: &RequestedInterval) -> NormalizedInterval {
    let (value, unit, rounded) = match requested.unit {
        RequestedIntervalUnit::Seconds => ((requested.value / 60.0).ceil().max(1.0), EffectiveIntervalUnit::Minutes, true),
        RequestedIntervalUnit::Minutes if requested.value >= 60.0 => ((requested.value / 60.0).round(), EffectiveIntervalUnit::Hours, true),
        RequestedIntervalUnit::Minutes => (requested.value, EffectiveIntervalUnit::Minutes, false),
        RequestedIntervalUnit::Hours if requested.value >= 24.0 => ((requested.value / 24.0).round(), EffectiveIntervalUnit::Days, true),
        RequestedIntervalUnit::Hours => (requested.value, EffectiveIntervalUnit::Hours, false),
        RequestedIntervalUnit::Days => (requested.value, EffectiveIntervalUnit::Days, false),
    };
    let (singular, plural, unit_ms) = match unit { EffectiveIntervalUnit::Minutes => ("minute", "minutes", 60_000.0), EffectiveIntervalUnit::Hours => ("hour", "hours", 3_600_000.0), EffectiveIntervalUnit::Days => ("day", "days", 86_400_000.0) };
    let human = format!("{value} {}", if (value - 1.0).abs() < f64::EPSILON { singular } else { plural });
    let rounding_notice = rounded.then(|| format!("Requested {} rounds to {human}.", requested.raw));
    NormalizedInterval { effective: EffectiveInterval { value, unit, human, rounded, rounding_notice }, interval_ms: value * unit_ms }
}
pub fn describe_cron(value: f64, unit: EffectiveIntervalUnit) -> String { match unit { EffectiveIntervalUnit::Minutes => format!("*/{value} * * * *"), EffectiveIntervalUnit::Hours => format!("0 */{value} * * *"), EffectiveIntervalUnit::Days => format!("0 0 */{value} * *") } }
pub fn compute_next_fire_at(now_ms: f64, interval_ms: f64) -> f64 { now_ms + interval_ms }
#[cfg(test)] mod tests {
    use super::*;
    fn req(value: f64, unit: RequestedIntervalUnit) -> RequestedInterval { RequestedInterval { value, unit, raw: format!("{value}{}", match unit { RequestedIntervalUnit::Seconds => "s", RequestedIntervalUnit::Minutes => "m", RequestedIntervalUnit::Hours => "h", RequestedIntervalUnit::Days => "d" }) } }
    fn check(value: f64, unit: RequestedIntervalUnit, expected: f64, effective: EffectiveIntervalUnit, rounded: bool, ms: f64) { let requested = req(value, unit); let result = normalize_interval(&requested); assert!((result.effective.value - expected).abs() < f64::EPSILON); assert_eq!(result.effective.unit, effective); assert_eq!(result.effective.rounded, rounded); assert!((result.interval_ms - ms).abs() < f64::EPSILON); assert_eq!(result.effective.rounding_notice.is_some(), rounded); }
    #[test] fn five_minutes_are_not_rounded() { check(5.0, RequestedIntervalUnit::Minutes, 5.0, EffectiveIntervalUnit::Minutes, false, 300_000.0); }
    #[test] fn forty_five_seconds_round_to_minute() { check(45.0, RequestedIntervalUnit::Seconds, 1.0, EffectiveIntervalUnit::Minutes, true, 60_000.0); }
    #[test] fn ninety_minutes_round_to_two_hours() { check(90.0, RequestedIntervalUnit::Minutes, 2.0, EffectiveIntervalUnit::Hours, true, 7_200_000.0); }
    #[test] fn seven_minutes_are_not_rounded() { check(7.0, RequestedIntervalUnit::Minutes, 7.0, EffectiveIntervalUnit::Minutes, false, 420_000.0); }
    #[test] fn one_hour_is_not_rounded() { check(1.0, RequestedIntervalUnit::Hours, 1.0, EffectiveIntervalUnit::Hours, false, 3_600_000.0); }
    #[test] fn thirty_six_hours_round_to_days() { check(36.0, RequestedIntervalUnit::Hours, 2.0, EffectiveIntervalUnit::Days, true, 172_800_000.0); }
    #[test] fn one_day_is_not_rounded() { check(1.0, RequestedIntervalUnit::Days, 1.0, EffectiveIntervalUnit::Days, false, 86_400_000.0); }
    #[test] fn minutes_round_half_up() { for (value, expected) in [(89.0, 1.0), (90.0, 2.0), (91.0, 2.0)] { check(value, RequestedIntervalUnit::Minutes, expected, EffectiveIntervalUnit::Hours, true, expected * 3_600_000.0); } }
    #[test] fn hours_round_half_up() { for (value, expected) in [(35.0, 1.0), (36.0, 2.0), (37.0, 2.0)] { check(value, RequestedIntervalUnit::Hours, expected, EffectiveIntervalUnit::Days, true, expected * 86_400_000.0); } }
    #[test] fn seconds_ceil_to_minimum_minute() { for (value, expected) in [(1.0, 1.0), (59.0, 1.0), (60.0, 1.0), (61.0, 2.0)] { check(value, RequestedIntervalUnit::Seconds, expected, EffectiveIntervalUnit::Minutes, true, expected * 60_000.0); } }
    #[test] fn interval_ms_tracks_effective_unit() { let requests = [req(5.0, RequestedIntervalUnit::Minutes), req(90.0, RequestedIntervalUnit::Minutes), req(36.0, RequestedIntervalUnit::Hours), req(1.0, RequestedIntervalUnit::Days)]; let result = requests.map(|r| normalize_interval(&r).interval_ms); assert_eq!(result, [300_000.0, 7_200_000.0, 172_800_000.0, 86_400_000.0]); }
    #[test] fn cron_forms_are_restricted() { let result = [describe_cron(5.0, EffectiveIntervalUnit::Minutes), describe_cron(2.0, EffectiveIntervalUnit::Hours), describe_cron(3.0, EffectiveIntervalUnit::Days)]; assert_eq!(result, ["*/5 * * * *", "0 */2 * * *", "0 0 */3 * *"]); }
    #[test] fn next_fire_uses_supplied_clock() { let now = 1_000_000.0; let result = compute_next_fire_at(now, 300_000.0); assert!((result - 1_300_000.0).abs() < f64::EPSILON); }
}
