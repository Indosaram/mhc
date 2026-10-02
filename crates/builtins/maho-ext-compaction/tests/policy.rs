use maho_core::compaction::settings::default_compaction_settings;
use maho_ext_compaction::policy::*;

fn close(actual: f64, expected: f64) { assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}"); }

macro_rules! tier {
    ($name:ident, $window:expr, $ratio:expr) => {
        #[test] fn $name() {
            // Given a model window, when its adaptive threshold is computed, then its tier matches.
            close(compute_adaptive_threshold_ratio($window, None), $ratio);
        }
    };
}
tier!(window_16k, 16000.0, 0.45);
tier!(window_32k, 32000.0, 0.5);
tier!(window_64k, 64000.0, 0.55);
tier!(window_128k, 128000.0, 0.6);
tier!(window_200k, 200000.0, 0.7);

#[test] fn effective_uses_adaptive_without_floor() { close(compute_effective_threshold(32000.0, None), 0.5); }
#[test] fn high_yield_clamps_at_floor() { close(compute_adaptive_threshold_ratio(16000.0, Some(9000.0)), 0.4); }
#[test] fn low_yield_cannot_raise_base() { close(compute_adaptive_threshold_ratio(16000.0, Some(500.0)), 0.45); }
#[test] fn effective_high_yield_uses_tokens_before() { close(compute_effective_threshold(32000.0, Some(CompactionYield { saved_tokens: 9000.0, tokens_before: 16000.0 })), 0.45); }
#[test] fn effective_low_yield_cannot_raise_base() { close(compute_effective_threshold(32000.0, Some(CompactionYield { saved_tokens: 500.0, tokens_before: 16000.0 })), 0.5); }
#[test] fn speculative_starts_at_three_quarters_of_each_tier() {
    let settings = default_compaction_settings();
    for (window, ratio) in [(16000.0, 0.45), (32000.0, 0.5), (64000.0, 0.55), (128000.0, 0.6), (200000.0, 0.7)] {
        let trigger = window * ratio * SPECULATIVE_FRACTION;
        assert!(!should_start_speculative_compaction(Some(trigger - 1.0), window, &settings, None, None));
        assert!(should_start_speculative_compaction(Some(trigger), window, &settings, None, None));
    }
}
#[test] fn small_window_keep_recent_leaves_summary_room() { close(compute_effective_keep_recent_tokens(20000.0, 16000.0, 0.45, 0.05), 8000.0); }
#[test] fn hard_limit_includes_additional_tokens() {
    assert!(!is_at_hard_limit(Some(83000.0), 100000.0, 16384.0, 0.0));
    assert!(is_at_hard_limit(Some(83000.0), 100000.0, 16384.0, 616.0));
}
#[test] fn large_window_tiers() {
    for (window, ratio) in [(256000.0, 0.7), (512000.0, 0.7), (1000000.0, 0.8)] { close(compute_adaptive_threshold_ratio(window, None), ratio); }
}
#[test] fn threshold_feedback_stays_in_bounds() {
    for window in [0.0, -1.0, 1.0, 1000.0, 16000.0, 16001.0, 32000.0, 64000.0, 128000.0, 200000.0, 512000.0, 512001.0, 1000000.0, 2000000.0, 9007199254740991.0] {
        for (saved, before) in [(0.0,0.0),(0.0,1.0),(1.0,0.0),(1.0,-1.0),(0.0,1000000.0),(500.0,500000.0),(9000.0,10000.0),(1000000.0,1000.0),(-1000000.0,1000.0),(9007199254740991.0,1.0)] {
            assert!((0.4..=0.85).contains(&compute_effective_threshold(window, Some(CompactionYield { saved_tokens: saved, tokens_before: before }))));
        }
    }
}
#[test] fn million_window_low_feedback_never_exceeds_base() {
    for (saved, before) in [(0.0,0.0),(500.0,500000.0),(9000.0,10000.0),(-1000000.0,1000.0)] {
        assert!(compute_effective_threshold(1000000.0, Some(CompactionYield { saved_tokens: saved, tokens_before: before })) <= 0.8);
    }
}
#[test] fn absent_yield_cannot_return_token_count() { close(compute_effective_threshold(1000000.0, None), 0.8); }
#[test] fn trigger_remains_below_window() {
    for window in [16000.0,200000.0,1000000.0,2000000.0] { assert!(window * compute_effective_threshold(window, None) < window); }
}
#[test] fn scaled_reserve_has_ceiling() {
    for (window, expected) in [(200000.0,16384.0),(1000000.0,40000.0),(2000000.0,49152.0)] { close(resolve_reserve_tokens(window,16384.0),expected); }
}
#[test] fn effective_reserve_scales_by_default() { close(resolve_effective_reserve_tokens(1000000.0,16384.0,None),40000.0); }
#[test] fn disabled_reserve_scaling_uses_configured_value() { close(resolve_effective_reserve_tokens(1000000.0,16384.0,Some(false)),16384.0); }
#[test] fn reserve_scaling_is_idempotent() {
    let once = resolve_effective_reserve_tokens(1000000.0,16384.0,None);
    close(resolve_effective_reserve_tokens(1000000.0,once,None),once);
}
#[test] fn million_low_yield_keeps_base() { close(compute_adaptive_threshold_ratio(1000000.0,Some(500.0)),0.8); }
#[test] fn million_threshold_recovers_after_high_yield() {
    close(compute_effective_threshold(1000000.0,Some(CompactionYield {saved_tokens:9000.0,tokens_before:10000.0})),0.75);
    close(compute_effective_threshold(1000000.0,Some(CompactionYield {saved_tokens:500.0,tokens_before:500000.0})),0.8);
}
#[test] fn half_million_high_yield_lowers_threshold() { close(compute_effective_threshold(512000.0,Some(CompactionYield {saved_tokens:9000.0,tokens_before:10000.0})),0.65); }
#[test] fn lead_aware_trigger_has_inclusive_boundary() {
    let settings = default_compaction_settings();
    assert!(!should_start_speculative_compaction(Some(119999.0),200000.0,&settings,None,Some(20000.0)));
    assert!(should_start_speculative_compaction(Some(120000.0),200000.0,&settings,None,Some(20000.0)));
}
#[test] fn no_lead_uses_legacy_fraction() {
    let settings = default_compaction_settings();
    assert!(!should_start_speculative_compaction(Some(104999.0),200000.0,&settings,None,None));
    assert!(should_start_speculative_compaction(Some(105000.0),200000.0,&settings,None,None));
}
#[test] fn million_window_scales_keep_recent() { close(compute_effective_keep_recent_tokens(20000.0,1000000.0,0.8,0.05),50000.0); }
