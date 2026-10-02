use maho_ext_pi_rules::rules::{ordering::{compare_candidates, sort_candidates}, types::RuleCandidate};
use std::cmp::Ordering;
fn candidate(source: &str, path: &str, distance: usize, global: bool) -> RuleCandidate {
    RuleCandidate { path: path.into(), real_path: path.into(), source: source.into(), distance, is_global: global, is_single_file: false, relative_path: path.into() }
}
fn check_order(a: RuleCandidate, b: RuleCandidate) {
    let input = [b.clone(), a.clone()];
    let result = sort_candidates(&input);
    assert_eq!(result, [a,b]);
}
#[test] fn local_before_global() { check_order(candidate(".omo/rules","local",0,false),candidate("~/.omo/rules","",9999,true)); }
#[test] fn closest_distance_first() { let far=candidate(".omo/rules","far",3,false); let close=candidate(".omo/rules","close",0,false); let mid=candidate(".omo/rules","mid",1,false); let result=sort_candidates(&[far.clone(),close.clone(),mid.clone()]); assert_eq!(result,[close,mid,far]); }
#[test] fn omo_before_claude() { check_order(candidate(".omo/rules","a",0,false),candidate(".claude/rules","b",0,false)); }
#[test] fn alphabetical_paths() { check_order(candidate(".omo/rules","alpha",0,false),candidate(".omo/rules","zebra",0,false)); }
#[test] fn equal_candidates_are_stable() { let a=candidate(".omo/rules","same",0,false); let mut b=a.clone(); b.path="second".into(); let result=sort_candidates(&[b.clone(),a.clone()]); assert_eq!(result,[b,a]); }
#[test] fn copilot_before_agents() { check_order(candidate(".github/copilot-instructions.md","a",0,false),candidate("AGENTS.md","b",0,false)); }
#[test] fn global_never_precedes_local_by_distance() { check_order(candidate(".omo/rules","local",0,false),candidate("~/.omo/rules","global",0,true)); }
#[test] fn home_sources_ordered() { check_order(candidate("~/.omo/rules","a",9999,true),candidate("~/.claude/rules","b",9999,true)); }
#[test] fn input_is_not_mutated() { let input=[candidate(".omo/rules","zebra",0,false),candidate(".omo/rules","alpha",0,false)]; let original=input.clone(); let result=sort_candidates(&input); assert_eq!(input,original); assert_ne!(result,input); }
#[test] fn empty_input() { let result=sort_candidates(&[]); assert!(result.is_empty()); }
#[test] fn unknown_source_last() { check_order(candidate("CONTEXT.md","a",0,false),candidate("missing/rules","b",0,false)); }
#[test] fn root_single_file_order() { let a=candidate("AGENTS.md","AGENTS.md",0,false); let b=candidate("CLAUDE.md","CLAUDE.md",0,false); let c=candidate("CONTEXT.md","CONTEXT.md",0,false); let result=sort_candidates(&[c.clone(),b.clone(),a.clone()]); assert_eq!(result,[a,b,c]); }
#[test] fn earlier_comparison_negative() { let a=candidate(".omo/rules","a",0,false); let b=candidate(".omo/rules","b",0,false); let result=compare_candidates(&a,&b); assert_eq!(result,Ordering::Less); }
#[test] fn identical_comparison_equal() { let a=candidate(".omo/rules","a",0,false); let result=compare_candidates(&a,&a); assert_eq!(result,Ordering::Equal); }
