use ulw_loop_core::cli_steering::{normalize_steering_proposal, parse_steering_kind, parse_steering_proposal};
use ulw_loop_core::steering_types::UlwLoopSteeringProposal;

fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).into()).collect() }
fn revise_args(extra: &[&str]) -> Vec<String> {
    let mut result = args(&["--kind", "revise_criterion", "--goal-id", "G001", "--criterion-id", "C002"]);
    result.extend(args(extra));
    result.extend(args(&["--evidence", "x", "--rationale", "y"]));
    result
}
fn proposal() -> UlwLoopSteeringProposal {
    UlwLoopSteeringProposal { kind: "revise_criterion".into(), source: "cli".into(), goal_id: Some(" G001 ".into()), target_goal_id: Some(" G001 ".into()), criterion_id: Some(" C002 ".into()), evidence: " x ".into(), rationale: " y ".into(), scenario: Some(" z ".into()), ..Default::default() }
}

#[test] fn parse_steering_kind_returns_valid_kind_from_kind() { assert_eq!(parse_steering_kind(&args(&["--kind", "add_subgoal"])).unwrap(), "add_subgoal"); }
#[test] fn parse_steering_kind_accepts_revise_criterion() { assert_eq!(parse_steering_kind(&args(&["--kind", "revise_criterion"])).unwrap(), "revise_criterion"); }
#[test] fn parse_steering_kind_throws_when_missing() { assert!(parse_steering_kind(&[]).is_err()); }
#[test] fn parse_steering_kind_throws_when_unknown() { assert!(parse_steering_kind(&args(&["--kind", "bogus"])).is_err()); }
#[test] fn parse_steering_source_defaults_to_cli() { assert_eq!(ulw_loop_core::cli_steering::parse_steering_source(&[]).unwrap(), "cli"); }
#[test] fn parse_steering_source_returns_explicit_value() { assert_eq!(ulw_loop_core::cli_steering::parse_steering_source(&args(&["--source", "user_prompt_submit"])).unwrap(), "user_prompt_submit"); }
#[test] fn parse_add_subgoal_builds_from_required_flags() { let p = parse_steering_proposal(&args(&["--kind","add_subgoal","--title"," New ","--objective"," Build ","--evidence"," x ","--rationale"," y "])).unwrap(); assert_eq!((p.kind, p.title.unwrap(), p.objective.unwrap(), p.evidence, p.rationale), ("add_subgoal".into(), "New".into(), "Build".into(), "x".into(), "y".into())); }
#[test] fn parse_add_subgoal_throws_when_title_missing() { assert!(parse_steering_proposal(&args(&["--kind","add_subgoal","--objective","Build","--evidence","x","--rationale","y"])).is_err()); }
#[test] fn parse_add_subgoal_throws_when_evidence_missing() { assert!(parse_steering_proposal(&args(&["--kind","add_subgoal","--title","New","--objective","Build","--rationale","y"])).is_err()); }
#[test] fn parse_revise_criterion_builds_required_fields() { let p = parse_steering_proposal(&revise_args(&["--scenario", "new scenario"])).unwrap(); assert_eq!((p.kind, p.target_goal_id.unwrap(), p.criterion_id.unwrap(), p.scenario.unwrap()), ("revise_criterion".into(), "G001".into(), "C002".into(), "new scenario".into())); }
#[test] fn parse_revise_criterion_accepts_expected_evidence() { let p = parse_steering_proposal(&revise_args(&["--expected-evidence", "new evidence"])).unwrap(); assert_eq!(p.expected_evidence.unwrap(), "new evidence"); }
#[test] fn parse_revise_criterion_accepts_user_model() { let p = parse_steering_proposal(&revise_args(&["--user-model", "edge"])).unwrap(); assert_eq!(p.user_model.unwrap(), "edge"); }
#[test] fn parse_revise_criterion_requires_update_field() { assert!(parse_steering_proposal(&revise_args(&[])).is_err()); }
#[test] fn parse_revise_criterion_requires_goal_id() { assert!(parse_steering_proposal(&args(&["--kind","revise_criterion","--criterion-id","C002","--scenario","s","--evidence","x","--rationale","y"])).is_err()); }
#[test] fn parse_revise_criterion_requires_criterion_id() { assert!(parse_steering_proposal(&args(&["--kind","revise_criterion","--goal-id","G001","--scenario","s","--evidence","x","--rationale","y"])).is_err()); }
#[test] fn parse_split_reads_inline_children() { let p = parse_steering_proposal(&args(&["--kind","split_subgoal","--goal-id","G001","--children",r#"[{"title":"A","objective":"Do A"}]"#,"--evidence","x","--rationale","y"])).unwrap(); assert_eq!(p.child_goals.unwrap()[0].title, "A"); }
#[test] fn parse_reorder_reads_inline_order() { let p = parse_steering_proposal(&args(&["--kind","reorder_pending","--order",r#"["G002","G001"]"#,"--evidence","x","--rationale","y"])).unwrap(); assert_eq!(p.pending_order.unwrap(), vec!["G002", "G001"]); }
#[test] fn parse_remaining_kinds_builds_revise_pending() { let p = parse_steering_proposal(&args(&["--kind","revise_pending_wording","--goal-id","G001","--title","New","--evidence","x","--rationale","y"])).unwrap(); assert_eq!((p.kind, p.target_goal_id.unwrap(), p.revised_title.unwrap()), ("revise_pending_wording".into(), "G001".into(), "New".into())); }
#[test] fn parse_remaining_kinds_builds_mark_blocked() { let p = parse_steering_proposal(&args(&["--kind","mark_blocked_superseded","--goal-id","G001","--replacements",r#"[{"title":"C","objective":"Do C"}]"#,"--evidence","x","--rationale","y"])).unwrap(); assert_eq!(p.child_goals.unwrap()[0].objective, "Do C"); }
#[test] fn normalize_trims_string_fields() { let p = normalize_steering_proposal(proposal()).unwrap(); assert_eq!((p.goal_id.unwrap(), p.target_goal_id.unwrap(), p.criterion_id.unwrap(), p.evidence, p.rationale, p.scenario.unwrap()), ("G001".into(), "G001".into(), "C002".into(), "x".into(), "y".into(), "z".into())); }
#[test] fn normalize_rejects_empty_evidence() { let mut p = proposal(); p.evidence = " ".into(); assert!(normalize_steering_proposal(p).is_err()); }
#[test] fn parse_annotate_ledger_builds_minimal_proposal() { let p = parse_steering_proposal(&args(&["--kind","annotate_ledger","--evidence","x","--rationale","y"])).unwrap(); assert_eq!((p.kind, p.source, p.evidence, p.rationale), ("annotate_ledger".into(), "cli".into(), "x".into(), "y".into())); }
