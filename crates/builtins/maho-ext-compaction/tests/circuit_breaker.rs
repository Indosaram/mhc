use maho_ext_api::CompactionReason;
use maho_ext_compaction::{circuit_breaker::*,state::*,per_turn_cap::*};
#[test] fn first_failure_does_not_trip() {let s=record_failure(create_initial_state(),0.0,Some(CompactionReason::Overflow),None);assert_eq!(s.consecutive_failures,1);assert!(!is_tripped(&s,0.0));}
#[test] fn third_failure_trips_and_notifies_once() {
    let mut notifications=Vec::new();let mut notify=|event|notifications.push(event);let mut s=create_initial_state();
    for now in [0.0,1.0,2.0,3.0] {s=record_failure(s,now,Some(CompactionReason::Overflow),Some(&mut notify));}
    assert_eq!(s.tripped_at,Some(2.0));assert_eq!(notifications.len(),1);assert_eq!(notifications[0].failure_count,3);
}
#[test] fn automatic_compaction_is_blocked_in_cooldown() {let s=CompactionExtensionState {consecutive_failures:3,tripped_at:Some(0.0),..create_initial_state()};assert!(is_tripped(&s,30000.0));}
#[test] fn manual_request_bypasses_cooldown() {let s=create_initial_state();assert!(should_bypass(&s,true,None));assert!(should_bypass(&s,false,Some(CompactionReason::Manual)));assert!(!should_bypass(&s,false,Some(CompactionReason::Overflow)));}
#[test] fn cooldown_is_active_at_59999ms() {let s=CompactionExtensionState {tripped_at:Some(0.0),..create_initial_state()};assert!(is_tripped(&s,59999.0));}
#[test] fn cooldown_expiry_restarts_failure_counter() {let s=CompactionExtensionState {consecutive_failures:3,tripped_at:Some(0.0),..create_initial_state()};assert!(!is_tripped(&s,60001.0));assert_eq!(record_failure(s,60001.0,None,None).consecutive_failures,1);}
#[test] fn successful_compaction_clears_failures() {let s=CompactionExtensionState {consecutive_failures:2,..create_initial_state()};assert_eq!(record_success(s).consecutive_failures,0);}
#[test] fn reload_resets_breaker() {assert!(!is_tripped(&create_initial_state(),30000.0));}
#[test] fn failures_share_counter_across_routes() {let mut s=create_initial_state();for route in [CompactionReason::Overflow,CompactionReason::Overflow,CompactionReason::Threshold] {s=record_failure(s,100.0,Some(route),None);}assert!(is_tripped(&s,100.0));}
#[test] fn fourth_required_compaction_is_admitted() {let mut s=create_initial_state();for _ in 0..3 {s=increment_accepted(s);}assert_eq!(s.accepted_this_turn,3);assert!(!should_reject_by_cap(&s).cancel);}
#[test] fn ineffective_attempts_do_not_consume_admission() {let mut s=create_initial_state();for _ in 0..3 {s=increment_ineffective(s);}assert_eq!(s.ineffective_attempts_this_turn,3);assert!(!should_reject_by_cap(&s).cancel);}
#[test] fn turn_reset_preserves_absolute_telemetry() {let mut s=create_initial_state();for _ in 0..3 {s=increment_accepted(s);}let s=reset_turn_counter(s,"turn-1");assert_eq!(s.accepted_this_turn,0);assert_eq!(s.accepted_absolute,3);assert!(!should_reject_by_cap(&s).cancel);}
#[test] fn long_lived_sessions_never_hit_absolute_cap() {let mut s=create_initial_state();for _ in 0..10000 {s=reset_turn_counter(increment_accepted(s),"turn");}assert_eq!(s.accepted_absolute,10000);assert!(!should_reject_by_cap(&s).cancel);}
#[test] fn reload_starts_with_open_admission() {let s=create_initial_state();assert_eq!(s.accepted_absolute,0);assert!(!should_reject_by_cap(&s).cancel);}
