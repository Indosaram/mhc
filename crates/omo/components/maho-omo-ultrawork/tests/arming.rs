use maho_omo_ultrawork::*;
use maho_omo_ultrawork::generated_directive::{SENPI_ASTRA_ULTRAWORK_DIRECTIVE, SENPI_ULTRAWORK_DIRECTIVE};
use maho_ext_api::AgentMessage;
#[test] fn trigger_corpus() { for s in ["ulw","ULW","ultrawork","ULTRAWORK","before ultrawork after","ulw-loop","ulw-plan."] { assert!(is_ultrawork_input(s)); } for s in ["ulwultrawork","하이ulw","ulw_helper.ts"] { assert!(!is_ultrawork_input(s)); } }
#[test] fn skill_family_word_mentions_follow_upstream_overlap_matching() { for s in ["ulw-plan","ulw-loop"] { assert!(is_ultrawork_input(s), "{s}"); } for s in ["","nulw-plan","ulw--plan","plan only"] { assert!(!is_ultrawork_input(s), "{s}"); } }
#[test] fn keyword_filter_rejects_references_quotes_and_relays() { for s in ["ulw_helper", "mass-ulw-refactor", "ulw-plan.md", ".omo/ulw/", "`ulw`", "> ulw", "[REPORT] ulw\nbody"] { assert!(!is_ultrawork_input(s), "{s}"); } for s in ["ulw", "please ulw", "ulw-loop", "ulw-plan."] { assert!(is_ultrawork_input(s), "{s}"); } }
#[test] fn astra_model_and_context_reselection() { assert_eq!(directive_for_model(Some("openai/gpt-6-astra:high")), SENPI_ASTRA_ULTRAWORK_DIRECTIVE); assert_eq!(directive_for_model(Some("openai/gpt-6-astra:custom")), SENPI_ULTRAWORK_DIRECTIVE); let mut messages=vec![AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage{content:maho_ai::types::UserContent::Text(SENPI_ULTRAWORK_DIRECTIVE.into()),timestamp:0}))]; assert!(reselect_context_directive(&mut messages,Some("gpt-6-astra"))); let AgentMessage::Llm(maho_ai::types::Message::User(user))=&messages[0] else {panic!("user")}; let maho_ai::types::UserContent::Text(text)=&user.content else {panic!("text")}; assert_eq!(text,SENPI_ASTRA_ULTRAWORK_DIRECTIVE); }
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
#[test] fn classification_occurrences() { for (text,ulw,ultrawork,count) in [("ulwultrawork",false,false,0),("ULW ulw Ultrawork",true,true,3),("ulw-plan",true,false,1)] { let c=classify_ultrawork_input(text,maho_ext_api::InputSource::Interactive,ArmingSnapshot::default());assert_eq!((c.matched_ulw,c.matched_ultrawork,c.occurrence_count),(ulw,ultrawork,count),"{text}"); } }
#[test] fn classification_stages() { for (snapshot,stage) in [(ArmingSnapshot::default(),"first_arm"),(ArmingSnapshot{was_armed:true,compact_rearm_pending:false},"remention"),(ArmingSnapshot{was_armed:false,compact_rearm_pending:true},"post_compact_rearm")] { assert_eq!(classify_ultrawork_input("ulw",maho_ext_api::InputSource::Interactive,snapshot).stage,stage); } }
#[test] fn classification_routes() { for (text,route,effective) in [("ulw","direct",true),("/skill:frontend ulw polish","skill_args",true),("/skill:myulw run it","none",false),("/skill:ultrawork fix it","skill_expansion",false),("<ultrawork-mode>rules</ultrawork-mode> ulw","embedded_directive",false)] { let c=classify_ultrawork_input(text,maho_ext_api::InputSource::Interactive,ArmingSnapshot::default());assert_eq!(c.route,route);assert_eq!(c.effective,effective); } }
#[test] fn classification_extension_suppressed() { assert_eq!(classify_ultrawork_input("ulw",maho_ext_api::InputSource::Extension,ArmingSnapshot::default()).suppression_reason,"extension_source"); }

#[test]
fn classification_fixed_corpus_matches_shipped_detector() {
    for text in ["", "ulw", "ULW", "ultrawork", "ulw-plan", "ulw-loop", "ulw-research",
        "ulwultrawork", "ULW ulw Ultrawork", "plan only", "하이ulw", "ulw_helper.ts",
        "before ultrawork after", "/skill:ultrawork", "/skill:frontend ulw polish", "/skill:myulw run it",
        "(ulw) [ultrawork] {ulw}", ".*+?^${}()|[]\\ ulw", "울트라워크", "nulw-plan", "ulw--plan", "ulw\nultrawork"] {
        let classification = classify_ultrawork_input(text, maho_ext_api::InputSource::Interactive, ArmingSnapshot::default());
        assert_eq!(classification.matched_ulw || classification.matched_ultrawork, is_ultrawork_input(text));
    }
    assert!(!is_ultrawork_input(&"x".repeat(100_000)));
}

#[test]
fn stale_snapshot_classification_is_pure() {
    let snapshot = ArmingSnapshot { was_armed: true, compact_rearm_pending: false };
    let first = classify_ultrawork_input("ULW ulw Ultrawork", maho_ext_api::InputSource::Interactive, snapshot);
    let second = classify_ultrawork_input("ULW ulw Ultrawork", maho_ext_api::InputSource::Interactive, snapshot);
    assert_eq!(first, second);
    assert_eq!(first.stage, "remention");
    assert_eq!(first.occurrence_count, 3);
}

#[test]
fn shared_snapshot_reads_leave_compact_pending_unchanged() {
    let id = "task-42-snapshot-read-only";
    let ledger = shared_session_arming();
    ledger.lock().expect("ledger").rearm_on_compact(Some(id));
    for _ in 0..3 {
        assert_eq!(arming_snapshot(Some(id)), ArmingSnapshot { was_armed: false, compact_rearm_pending: true });
    }
    assert!(ledger.lock().expect("ledger").is_compact_rearm_pending(Some(id)));
}
