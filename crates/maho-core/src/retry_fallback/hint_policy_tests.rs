use super::hint_policy::*;

const SETTINGS: HintPolicySettings = HintPolicySettings { hinted_wait_cap_ms: 300_000.0, probe_back_max_ms: 3_600_000.0 };

macro_rules! tier_test {
    ($name:ident, $hint:expr, $tier:ident) => {
        #[test]
        fn $name() {
            assert_eq!(classify_rate_limited_wait($hint, SETTINGS), HintTier::$tier);
        }
    };
}
tier_test!(missing_hint, None, NoHintFastFallback);
tier_test!(zero_hint, Some(0.0), Tier1InTurn);
tier_test!(below_cap, Some(299_999.0), Tier1InTurn);
tier_test!(at_cap, Some(300_000.0), Tier1InTurn);
tier_test!(above_cap, Some(300_001.0), Tier2FallbackProbeBack);
tier_test!(below_probe_max, Some(3_599_999.0), Tier2FallbackProbeBack);
tier_test!(at_probe_max, Some(3_600_000.0), Tier3FallbackOnly);
tier_test!(above_probe_max, Some(7_200_000.0), Tier3FallbackOnly);

#[test]
fn custom_thresholds() {
    let settings = HintPolicySettings { hinted_wait_cap_ms: 10_000.0, probe_back_max_ms: 60_000.0 };
    for (hint, tier) in [(10_000.0, HintTier::Tier1InTurn), (10_001.0, HintTier::Tier2FallbackProbeBack),
        (59_999.0, HintTier::Tier2FallbackProbeBack), (60_000.0, HintTier::Tier3FallbackOnly)] {
        assert_eq!(classify_rate_limited_wait(Some(hint), settings), tier);
    }
}

macro_rules! schedule_test {
    ($name:ident, $hint:expr, $now:expr, $first:expr, $deadline:expr) => {
        #[test]
        fn $name() {
            assert_eq!(probe_back_schedule($hint, $now), ProbeBackSchedule { first_at_ms: $first, deadline_ms: $deadline });
        }
    };
}
schedule_test!(half_hint, 10_000.0, 1_000.0, 6_000.0, 11_000.0);
schedule_test!(odd_hint, 7_001.0, 0.0, 3_501.0, 7_001.0);
schedule_test!(zero_schedule, 0.0, 5_000.0, 5_000.0, 5_000.0);
schedule_test!(large_schedule, 3_600_000.0, 0.0, 1_800_000.0, 3_600_000.0);

macro_rules! delay_test {
    ($name:ident, $phase:ident, $deadline:expr, $attempt:expr, $total:expr, $hint:expr, $cap:expr, $now:expr,
        $delay:expr, $next:ident, $next_deadline:expr, $next_total:expr, $demote:expr) => {
        #[test]
        fn $name() {
            let state = InTurnState { probe_phase: ProbePhase::$phase, hint_deadline_ms: $deadline,
                attempt: $attempt, cumulative_hinted_wait_ms: $total };
            let actual = next_in_turn_delay_ms(state, $hint, 2_000.0, $cap, $now);
            assert_eq!(actual, InTurnResult { delay_ms: $delay, probe_phase: ProbePhase::$next,
                hint_deadline_ms: $next_deadline, cumulative_hinted_wait_ms: $next_total, demote_to_probe_back: $demote });
        }
    };
}
delay_test!(first_hint, Idle, None, 1, 0.0, Some(120_000.0), 300_000.0, 1_000.0, 60_000.0, HalfUsed, Some(121_000.0), 60_000.0, false);
delay_test!(first_odd_hint, Idle, None, 1, 0.0, Some(7_001.0), 300_000.0, 0.0, 3_501.0, HalfUsed, Some(7_001.0), 3_501.0, false);
delay_test!(first_zero_hint, Idle, None, 1, 0.0, Some(0.0), 300_000.0, 5_000.0, 2_000.0, HalfUsed, Some(5_000.0), 2_000.0, false);
delay_test!(short_hint_floor, Idle, None, 1, 0.0, Some(3_000.0), 300_000.0, 0.0, 2_000.0, HalfUsed, Some(3_000.0), 2_000.0, false);
delay_test!(long_hint_wins, Done, None, 3, 0.0, Some(30_000.0), 300_000.0, 0.0, 30_000.0, Done, None, 0.0, false);
delay_test!(elapsed_deadline_floor, HalfUsed, Some(61_000.0), 2, 60_000.0, None, 300_000.0, 61_000.0, 4_000.0, Done, Some(61_000.0), 64_000.0, false);
delay_test!(remaining_deadline, HalfUsed, Some(100_000.0), 2, 50_000.0, None, 300_000.0, 30_000.0, 70_000.0, Done, Some(100_000.0), 120_000.0, false);
delay_test!(new_hint_deadline, HalfUsed, Some(100_000.0), 2, 50_000.0, Some(80_000.0), 300_000.0, 30_000.0, 80_000.0, Done, Some(110_000.0), 130_000.0, false);
delay_test!(growing_hint, HalfUsed, Some(100_000.0), 2, 50_000.0, Some(200_000.0), 300_000.0, 50_000.0, 200_000.0, Done, Some(250_000.0), 250_000.0, false);
delay_test!(shrinking_hint, HalfUsed, Some(500_000.0), 2, 250_000.0, Some(100_000.0), 300_000.0, 250_000.0, 100_000.0, Done, Some(350_000.0), 350_000.0, true);
delay_test!(shortened_deadline, HalfUsed, Some(500_000.0), 2, 300_000.0, Some(10_000.0), 300_000.0, 320_000.0, 10_000.0, Done, Some(330_000.0), 310_000.0, true);
delay_test!(zero_after_half, HalfUsed, Some(100_000.0), 2, 50_000.0, Some(0.0), 300_000.0, 60_000.0, 4_000.0, Done, Some(60_000.0), 54_000.0, false);
delay_test!(unhinted_remaining, HalfUsed, Some(100_000.0), 2, 50_000.0, None, 300_000.0, 60_000.0, 40_000.0, Done, Some(100_000.0), 90_000.0, false);
delay_test!(non_rate_limit_backoff, Done, Some(100_000.0), 3, 50_000.0, None, 300_000.0, 60_000.0, 8_000.0, Done, Some(100_000.0), 50_000.0, false);
delay_test!(fresh_short_hint_floor, Done, None, 3, 120_000.0, Some(5_000.0), 300_000.0, 200_000.0, 8_000.0, Done, None, 120_000.0, false);
delay_test!(second_attempt_backoff, Done, None, 2, 120_000.0, None, 300_000.0, 200_000.0, 4_000.0, Done, None, 120_000.0, false);
delay_test!(fourth_attempt_backoff, Done, None, 4, 120_000.0, None, 300_000.0, 200_000.0, 16_000.0, Done, None, 120_000.0, false);
delay_test!(cumulative_over_cap, Idle, None, 1, 290_000.0, Some(120_000.0), 300_000.0, 1_000.0, 60_000.0, HalfUsed, Some(121_000.0), 350_000.0, true);
delay_test!(cumulative_at_cap, Idle, None, 1, 240_000.0, Some(120_000.0), 300_000.0, 1_000.0, 60_000.0, HalfUsed, Some(121_000.0), 300_000.0, false);
delay_test!(deadline_over_cap, HalfUsed, Some(100_000.0), 2, 250_001.0, None, 300_000.0, 50_000.0, 50_000.0, Done, Some(100_000.0), 300_001.0, true);
delay_test!(exponential_not_counted, Done, None, 5, 299_000.0, None, 300_000.0, 500_000.0, 32_000.0, Done, None, 299_000.0, false);
delay_test!(hint_at_cap, Idle, None, 1, 0.0, Some(300_000.0), 300_000.0, 0.0, 150_000.0, HalfUsed, Some(300_000.0), 150_000.0, false);
delay_test!(second_probe_at_cap, HalfUsed, Some(300_000.0), 2, 150_000.0, None, 300_000.0, 150_000.0, 150_000.0, Done, Some(300_000.0), 300_000.0, false);
delay_test!(second_probe_over_cap, HalfUsed, Some(300_000.0), 2, 150_001.0, None, 300_000.0, 150_000.0, 150_000.0, Done, Some(300_000.0), 300_001.0, true);
delay_test!(floored_demotion, HalfUsed, Some(10_000.0), 8, 40_000.0, None, 100_000.0, 10_000.0, 256_000.0, Done, Some(10_000.0), 296_000.0, true);

#[test]
fn exponential_never_restarts() {
    let state = InTurnState { probe_phase: ProbePhase::Done, hint_deadline_ms: None, attempt: 3, cumulative_hinted_wait_ms: 100_000.0 };
    assert_eq!(next_in_turn_delay_ms(state, None, 2_000.0, 300_000.0, 200_000.0).delay_ms, 8_000.0);
    assert_eq!(next_in_turn_delay_ms(InTurnState { attempt: 4, ..state }, None, 2_000.0, 300_000.0, 200_000.0).delay_ms, 16_000.0);
}

#[test]
fn repeated_short_hints_increase_pressure() {
    let mut state = InTurnState { probe_phase: ProbePhase::Idle, hint_deadline_ms: None, attempt: 1, cumulative_hinted_wait_ms: 0.0 };
    for (hint, now, expected) in [(Some(3_000.0), 0.0, 2_000.0), (None, 10_000.0, 4_000.0), (Some(5_000.0), 20_000.0, 8_000.0)] {
        let result = next_in_turn_delay_ms(state, hint, 2_000.0, 300_000.0, now);
        assert_eq!(result.delay_ms, expected);
        state = InTurnState { attempt: state.attempt + 1, probe_phase: result.probe_phase,
            hint_deadline_ms: result.hint_deadline_ms, cumulative_hinted_wait_ms: result.cumulative_hinted_wait_ms };
    }
}

#[test]
fn degraded_no_hint_backoff() {
    for (attempt, expected) in [(1, 2_000.0), (3, 8_000.0)] {
        assert_eq!(degrade_without_fallback(HintTier::NoHintFastFallback, None, attempt, 2_000.0, 300_000.0), DegradedRateLimitAction::InTurn { delay_ms: expected });
    }
}
#[test]
fn degraded_tier_two_cap() {
    assert_eq!(degrade_without_fallback(HintTier::Tier2FallbackProbeBack, Some(360_000.0), 1, 2_000.0, 300_000.0), DegradedRateLimitAction::InTurn { delay_ms: 300_000.0 });
}
#[test]
fn degraded_tier_two_floor() {
    assert_eq!(degrade_without_fallback(HintTier::Tier2FallbackProbeBack, Some(5_000.0), 4, 2_000.0, 5_000.0), DegradedRateLimitAction::InTurn { delay_ms: 16_000.0 });
}
#[test]
fn degraded_tier_three_terminal() {
    assert_eq!(degrade_without_fallback(HintTier::Tier3FallbackOnly, Some(3_600_000.0), 1, 2_000.0, 300_000.0), DegradedRateLimitAction::Fail { hint_ms: 3_600_000.0 });
}
