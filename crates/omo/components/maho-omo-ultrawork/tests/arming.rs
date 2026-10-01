use maho_omo_ultrawork::*;
#[test] fn trigger_corpus() { for s in ["ulw","ULW","ultrawork","ULTRAWORK","ulwultrawork","하이ulw","ulw_helper.ts","before ultrawork after"] { assert!(is_ultrawork_input(s)); } }
#[test] fn skill_family_not_trigger() { for s in ["","ulw-plan","ulw-loop","ulw-research","nulw-plan","ulw--plan","plan only"] { assert!(!is_ultrawork_input(s)); } }
#[test] fn first_arm() { let mut a=SessionArming::default(); assert!(!a.is_armed(Some("a"))); a.mark_armed(Some("a")); assert!(a.is_armed(Some("a"))); }
#[test] fn accepted_compact_rearms() { let mut a=SessionArming::default(); a.mark_armed(Some("a")); a.rearm_on_compact(Some("a")); assert!(!a.is_armed(Some("a"))); assert!(a.is_compact_rearm_pending(Some("a"))); }
#[test] fn arming_clears_pending() { let mut a=SessionArming::default(); a.rearm_on_compact(Some("a")); a.mark_armed(Some("a")); assert!(!a.is_compact_rearm_pending(Some("a"))); }
#[test] fn switch_preserves_armed_session() { let mut a=SessionArming::default(); a.mark_armed(Some("a")); a.track_session(Some("b")); a.track_session(Some("a")); assert!(a.is_armed(Some("a"))); }
#[test] fn independent_session() { let mut a=SessionArming::default(); a.mark_armed(Some("a")); assert!(!a.is_armed(Some("b"))); }
#[test] fn anonymous_session() { let mut a=SessionArming::default(); a.mark_armed(None); assert!(a.is_armed(None)); a.rearm_on_compact(None); assert!(!a.is_armed(None)); }
#[test] fn compact_targets_current() { let mut a=SessionArming::default(); a.track_session(Some("a")); a.mark_armed(Some("a")); a.rearm_on_compact(None); assert!(!a.is_armed(Some("a"))); assert!(a.is_compact_rearm_pending(Some("a"))); }
#[test] fn embedded_pair() { assert!(already_embedded("<ultrawork-mode>rules</ultrawork-mode> ulw")); assert!(!already_embedded("question <ultrawork-mode> ulw")); }
#[test] fn skill_args_trigger() { assert!(!skill_invocation_suppressed("/skill:frontend ulw polish")); }
#[test] fn skill_name_only_suppressed() { assert!(skill_invocation_suppressed("/skill:myulw run it")); }
#[test] fn full_skill_suppressed() { assert!(skill_invocation_suppressed("/skill:ultrawork fix it")); }
#[test] fn arming_reads_do_not_mutate() { let mut a=SessionArming::default(); a.rearm_on_compact(Some("a")); for _ in 0..3 { assert!(!a.is_armed(Some("a"))); assert!(a.is_compact_rearm_pending(Some("a"))); } }
#[test] fn process_shared_ledger() { assert!(std::sync::Arc::ptr_eq(&shared_session_arming(),&shared_session_arming())); }
