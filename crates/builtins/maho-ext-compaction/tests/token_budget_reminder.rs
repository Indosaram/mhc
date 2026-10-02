use maho_ext_compaction::token_budget_reminder::*;
fn reminder(tokens:f64,epoch:u64,state:TokenBudgetReminderState)->TokenBudgetReminderResult {
    compute_token_budget_reminder(TokenBudgetReminderInput {context_tokens:tokens,context_window:32000.0,threshold_tokens:10000.0,lead_tokens:500.0,compaction_epoch:epoch,state})
}
#[test] fn initial_state_has_no_epoch() { assert_eq!(create_initial_reminder_state().last_fired_epoch,None); }
#[test] fn in_zone_fires_once() { let r=reminder(9500.0,1,create_initial_reminder_state());assert!(r.message.is_some());assert_eq!(r.next_state.last_fired_epoch,Some(1));assert_eq!(r.next_state.lease.unwrap().message,r.message.unwrap()); }
#[test] fn upper_zone_boundary_is_inclusive() { assert!(reminder(9000.0,1,create_initial_reminder_state()).message.is_some()); }
#[test] fn same_epoch_does_not_refire() { let s=TokenBudgetReminderState {last_fired_epoch:Some(1),lease:None};assert!(reminder(9500.0,1,s).message.is_none()); }
#[test] fn new_epoch_refires() { let s=TokenBudgetReminderState {last_fired_epoch:Some(1),lease:None};let r=reminder(9500.0,2,s);assert!(r.message.is_some());assert_eq!(r.next_state.last_fired_epoch,Some(2)); }
#[test] fn next_turn_clears_active_lease() { let s=reminder(9500.0,1,create_initial_reminder_state()).next_state;let r=reminder(9500.0,1,s);assert!(r.message.is_none());assert!(r.next_state.lease.is_none());assert_eq!(r.next_state.last_fired_epoch,Some(1)); }
#[test] fn disable_clears_lease_and_preserves_epoch() { let s=reminder(9500.0,1,create_initial_reminder_state()).next_state;let r=clear_token_budget_reminder_lease(s);assert!(r.lease.is_none());assert_eq!(r.last_fired_epoch,Some(1)); }
#[test] fn above_zone_does_not_fire() { assert!(reminder(8999.0,1,create_initial_reminder_state()).message.is_none()); }
#[test] fn at_and_past_threshold_do_not_fire() { for tokens in [10000.0,10001.0] { assert!(reminder(tokens,1,create_initial_reminder_state()).message.is_none()); } }
#[test] fn unchanged_state_preserves_values() { let s=TokenBudgetReminderState {last_fired_epoch:Some(3),lease:None};assert_eq!(reminder(9500.0,3,s.clone()).next_state,s); }
