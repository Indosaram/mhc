use maho_core::compaction::settings::{CompactionSettings, default_compaction_settings};
use maho_ext_api::ExtensionMode;
use maho_ext_compaction::{idle::*, speculation_lead::*};
fn decision(settings: &CompactionSettings) -> IdleCompactionDecision<'_> {
    IdleCompactionDecision {will_retry:false,aborted:false,settings,tokens:Some(80000.0),context_window:100000.0,breaker_tripped:false,last_yield:None,mode:ExtensionMode::Tui}
}
#[test] fn idle_runs_over_threshold() { assert!(should_run_idle_compaction(&decision(&default_compaction_settings()))); }
#[test] fn idle_runs_in_persistent_modes() {
    let settings=default_compaction_settings();
    for mode in [ExtensionMode::Rpc,ExtensionMode::AppServer] { let mut d=decision(&settings);d.mode=mode;assert!(should_run_idle_compaction(&d)); }
}
#[test] fn idle_rejects_auto_continue() { let s=default_compaction_settings();let mut d=decision(&s);d.will_retry=true;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_aborted() { let s=default_compaction_settings();let mut d=decision(&s);d.aborted=true;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_print() { let s=default_compaction_settings();let mut d=decision(&s);d.mode=ExtensionMode::Print;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_json() { let s=default_compaction_settings();let mut d=decision(&s);d.mode=ExtensionMode::Json;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_disabled() { let mut s=default_compaction_settings();s.idle_compaction_enabled=Some(false);assert!(!should_run_idle_compaction(&decision(&s))); }
#[test] fn idle_rejects_tripped_breaker() { let s=default_compaction_settings();let mut d=decision(&s);d.breaker_tripped=true;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_missing_usage() { let s=default_compaction_settings();let mut d=decision(&s);d.tokens=None;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_unknown_tokens() { let s=default_compaction_settings();let mut d=decision(&s);d.tokens=None;assert!(!should_run_idle_compaction(&d)); }
#[test] fn idle_rejects_below_threshold() { let s=default_compaction_settings();let mut d=decision(&s);d.tokens=Some(20000.0);assert!(!should_run_idle_compaction(&d)); }
#[test] fn lead_clamps_at_floor_and_ceiling() {
    for (threshold,lead,expected) in [(50000.0,None,8192.0),(200000.0,None,25000.0),(800000.0,None,32768.0),(140000.0,Some(1.0),8192.0),(140000.0,Some(1000000.0),32768.0)] { assert!((resolve_speculation_lead_tokens(threshold,lead)-expected).abs()<1e-10); }
}
#[test] fn grace_band_respects_reserve() { assert!((resolve_grace_band_cap_tokens(200000.0,25000.0,210000.0,10000.0)-200000.0).abs()<1e-10); }
#[test] fn grace_band_is_exclusive() { assert!(!is_within_grace_band(112500.0,100000.0,12500.0,200000.0,10000.0));assert!(is_within_grace_band(112499.0,100000.0,12500.0,200000.0,10000.0)); }
#[test] fn warming_starts_at_half_window() { let s=default_compaction_settings();let mut d=decision(&s);d.tokens=Some(49900.0);assert!(!should_warm_at_idle(&d));d.tokens=Some(50000.0);assert!(should_warm_at_idle(&d)); }
#[test] fn warming_preserves_eligibility_guards() { let s=default_compaction_settings();let mut d=decision(&s);d.will_retry=true;assert!(!should_warm_at_idle(&d));d.will_retry=false;d.aborted=true;assert!(!should_warm_at_idle(&d)); }
#[test] fn warm_staleness_is_exclusive() { assert!(!is_warm_result_stale(50000.0,58192.0,1000.0));assert!(is_warm_result_stale(50000.0,58193.0,1000.0));assert!(!is_warm_result_stale(50000.0,70000.0,20000.0));assert!(is_warm_result_stale(50000.0,70001.0,20000.0)); }
