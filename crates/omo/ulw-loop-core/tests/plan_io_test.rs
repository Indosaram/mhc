#![allow(clippy::unwrap_used)]
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::*;
use ulw_loop_core::paths::{ulw_loop_dir, ulw_loop_goals_path, ulw_loop_ledger_path, UlwLoopScope};
use std::fs::{self, OpenOptions};
use std::io::Write;

fn sample_plan() -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        brief: Some("sample brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id: None,
        goals: vec![],
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
        codex_goal_mode: None,
    }
}

#[test]
fn test_write_and_read_plan() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = sample_plan();
    write_plan(tmp.path(), plan.clone(), None)?;

    let read_back = read_ulw_loop_plan(tmp.path(), None)?;
    assert_eq!(read_back.brief, plan.brief);
    assert_eq!(read_back.version, plan.version);
    Ok(())
}

#[test]
fn test_read_nonexistent_plan_fails() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_ulw_loop_plan(tmp.path(), None).is_err());
}

fn ledger_entry(kind: &str) -> UlwLoopLedgerEntry { UlwLoopLedgerEntry { at: "2026-05-23T00:00:00Z".into(), kind: kind.into(), goal_id: Some("G001".into()), ..Default::default() } }

#[test]
fn test_missing_plan_error_mentions_create_command() { let tmp=tempfile::tempdir().unwrap(); let e=read_ulw_loop_plan(tmp.path(),None).unwrap_err(); assert!(e.to_string().contains("create-goals")); }
#[test]
fn test_fixture_like_plan_parses() { let tmp=tempfile::tempdir().unwrap(); let mut p=sample_plan(); p.goals=vec![ulw_loop_core::plan_goal_factory::make_goal("one","one",0,"2026-05-23T00:00:00Z",None).unwrap()]; write_plan(tmp.path(),p,None).unwrap(); let got=read_ulw_loop_plan(tmp.path(),None).unwrap(); assert_eq!(got.version,1); assert_eq!(got.goals[0].success_criteria.len(),3); }
#[test]
fn test_legacy_objective_migrates_under_lock() { let tmp=tempfile::tempdir().unwrap(); let old="Complete all ulw-loop stories in .omo/ulw-loop/goals.json: G001 Build auth service"; let mut p=sample_plan(); p.codex_goal_mode=Some("aggregate".into()); p.codex_objective=Some(old.into()); write_plan(tmp.path(),p,None).unwrap(); let got=with_ulw_loop_mutation_lock(tmp.path(),None,None,||{let x=read_ulw_loop_plan(tmp.path(),None)?; write_plan(tmp.path(),x.clone(),None)?; Ok(x)}).unwrap(); assert_ne!(got.codex_objective.as_deref(),Some(old)); assert_eq!(got.codex_objective_aliases.unwrap(),vec![old]); }
#[test]
fn test_legacy_objective_read_persists_migration() { let tmp=tempfile::tempdir().unwrap(); let old="Complete all ulw-loop stories in .omo/ulw-loop/goals.json: G001 Build auth service"; let mut p=sample_plan(); p.codex_goal_mode=Some("aggregate".into()); p.codex_objective=Some(old.into()); write_raw(tmp.path(),p); let got=read_ulw_loop_plan(tmp.path(),None).unwrap(); assert_ne!(got.codex_objective.as_deref(),Some(old)); assert_ne!(got.updated_at,"2026-05-23T00:00:00Z"); let persisted=serde_json::from_str::<UlwLoopPlan>(&fs::read_to_string(ulw_loop_goals_path(tmp.path(),None)).unwrap()).unwrap(); assert_eq!(persisted.codex_objective,got.codex_objective); let migrated=read_ledger(tmp.path(),None).into_iter().filter(|e|e.kind=="aggregate_objective_migrated").collect::<Vec<_>>(); assert_eq!(migrated.len(),1); assert_eq!(migrated[0].before.as_ref().unwrap()["codexObjective"],old); }
#[test]
fn test_objective_containing_legacy_prefix_mid_string_is_not_migrated() { let tmp=tempfile::tempdir().unwrap(); let obj="Note: Complete all ulw-loop stories in .omo/ulw-loop/goals.json: G001"; let mut p=sample_plan(); p.codex_goal_mode=Some("aggregate".into()); p.codex_objective=Some(obj.into()); write_raw(tmp.path(),p); assert_eq!(read_ulw_loop_plan(tmp.path(),None).unwrap().codex_objective.as_deref(),Some(obj)); assert!(read_ledger(tmp.path(),None).iter().all(|e|e.kind!="aggregate_objective_migrated")); }
#[test]
fn test_migrated_plan_reads_without_ledger() { let tmp=tempfile::tempdir().unwrap(); let mut p=sample_plan(); p.codex_goal_mode=Some("aggregate".into()); p.codex_objective=Some("objective".into()); write_raw(tmp.path(),p); assert_eq!(read_ulw_loop_plan(tmp.path(),None).unwrap().codex_objective.as_deref(),Some("objective")); assert!(!ulw_loop_ledger_path(tmp.path(),None).exists()); }
fn write_raw(root:&std::path::Path,p:UlwLoopPlan){fs::create_dir_all(ulw_loop_dir(root,None)).unwrap();fs::write(ulw_loop_goals_path(root,None),format!("{}\n",serde_json::to_string_pretty(&p).unwrap())).unwrap();}
#[test]
fn test_write_plan_leaves_no_tmp() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); assert!(!fs::read_dir(ulw_loop_dir(tmp.path(),None)).unwrap().any(|e|e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))); }
#[test]
fn test_write_plan_overwrites() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut p=read_ulw_loop_plan(tmp.path(),None).unwrap(); p.codex_objective=Some("second".into()); write_plan(tmp.path(),p,None).unwrap(); assert_eq!(read_ulw_loop_plan(tmp.path(),None).unwrap().codex_objective.as_deref(),Some("second")); }
#[test]
fn test_append_single_entry() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); append_ledger(tmp.path(),ledger_entry("goal_started"),None).unwrap(); assert_eq!(read_ledger(tmp.path(),None).len(),1); }
#[test]
fn test_append_creates_ledger() { let tmp=tempfile::tempdir().unwrap(); write_raw(tmp.path(),sample_plan()); append_ledger(tmp.path(),ledger_entry("goal_completed"),None).unwrap(); assert!(fs::read_to_string(ulw_loop_ledger_path(tmp.path(),None)).unwrap().contains("goal_completed")); }
#[test]
fn test_append_preserves_entries() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); append_ledger(tmp.path(),ledger_entry("goal_started"),None).unwrap(); append_ledger(tmp.path(),ledger_entry("goal_completed"),None).unwrap(); assert_eq!(read_ledger(tmp.path(),None).iter().map(|x|x.kind.as_str()).collect::<Vec<_>>(),["goal_started","goal_completed"]); }
#[test]
fn test_read_steering_filters() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); for k in ["steering_accepted","goal_started","steering_rejected","criteria_revised"] { append_ledger(tmp.path(),ledger_entry(k),None).unwrap(); } assert_eq!(read_steering_ledger_entries(tmp.path(),None).iter().map(|x|x.kind.as_str()).collect::<Vec<_>>(),["steering_accepted","steering_rejected","criteria_revised"]); }
#[test]
fn test_read_steering_missing_empty() { assert!(read_steering_ledger_entries(tempfile::tempdir().unwrap().path(),None).is_empty()); }
fn accepted(key:&str)->UlwLoopLedgerEntry { let mut e=ledger_entry("steering_accepted"); e.idempotency_key=Some(key.into()); e.steering=Some(ulw_loop_core::steering_types::UlwLoopSteeringAudit{kind:"steering_accepted".into(),source:"cli".into(),target_goal_ids:vec!["G001".into()],criterion_id:None,before:None,after:None,evidence:"observed".into(),rationale:"needed".into(),invariant:ulw_loop_core::steering_types::UlwLoopSteeringInvariantResult{accepted:true,structural_invariant_accepted:true,evidence_backed_necessity:true,no_easier_completion:true,rejected_reasons:vec![],reasons:None},directive_text:None,prompt_signature:None,idempotency_key:Some(key.into()),deduped:None}); e }
#[test]
fn test_find_accepted_top_level_key() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); append_ledger(tmp.path(),accepted("needle-key"),None).unwrap(); assert_eq!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).unwrap().idempotency_key.as_deref(),Some("needle-key")); }
#[test]
fn test_find_accepted_nested_key() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut e=accepted("other"); e.idempotency_key=None; append_ledger(tmp.path(),e,None).unwrap(); assert!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).is_none()); }
#[test]
fn test_rejected_not_found() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut e=accepted("needle-key"); e.kind="steering_rejected".into(); e.steering.as_mut().unwrap().invariant.accepted=false; append_ledger(tmp.path(),e,None).unwrap(); assert!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).is_none()); }
#[test]
fn test_rejected_before_accepted() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut r=accepted("needle-key"); r.kind="steering_rejected".into(); r.steering.as_mut().unwrap().invariant.accepted=false; append_ledger(tmp.path(),r,None).unwrap(); append_ledger(tmp.path(),accepted("needle-key"),None).unwrap(); assert!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).unwrap().steering.unwrap().invariant.accepted); }
#[test]
fn test_missing_accepted_none() { assert!(find_accepted_steering_ledger_entry(tempfile::tempdir().unwrap().path(),"needle-key",None).is_none()); }
#[test]
fn test_nonsteering_key_ignored() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut e=ledger_entry("goal_completed"); e.idempotency_key=Some("needle-key".into()); append_ledger(tmp.path(),e,None).unwrap(); assert!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).is_none()); }
#[test]
fn test_evidence_text_key_ignored() { let tmp=tempfile::tempdir().unwrap(); write_plan(tmp.path(),sample_plan(),None).unwrap(); let mut e=accepted("other"); e.steering.as_mut().unwrap().evidence="needle-key".into(); append_ledger(tmp.path(),e,None).unwrap(); assert!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).is_none()); }
#[test]
fn test_legacy_large_line_key_found() { let tmp=tempfile::tempdir().unwrap(); fs::create_dir_all(ulw_loop_dir(tmp.path(),None)).unwrap(); let mut e=accepted("needle-key"); e.before=Some(serde_json::json!({"detail":"x".repeat(100_001)})); let line=serde_json::to_string(&e).unwrap(); let mut f=OpenOptions::new().create(true).append(true).open(ulw_loop_ledger_path(tmp.path(),None)).unwrap(); writeln!(f,"{line}").unwrap(); assert!(line.len()>100_000); assert_eq!(find_accepted_steering_ledger_entry(tmp.path(),"needle-key",None).unwrap().kind,"steering_accepted"); }
#[test]
fn test_lock_serializes_invocations() { let tmp=tempfile::tempdir().unwrap(); let p=tmp.path().join("counter"); fs::write(&p,"0").unwrap(); for _ in 0..3 { with_ulw_loop_mutation_lock(tmp.path(),None,None,||{let n:usize=fs::read_to_string(&p).unwrap().parse().unwrap(); fs::write(&p,(n+1).to_string()).unwrap(); Ok::<(), ulw_loop_core::runtime::UlwLoopError>(())}).unwrap(); } assert_eq!(fs::read_to_string(p).unwrap(),"3"); }
#[test]
fn test_lock_propagates_and_recovers() { let tmp=tempfile::tempdir().unwrap(); let e=with_ulw_loop_mutation_lock(tmp.path(),None,None,||Err::<&str, _>(ulw_loop_core::runtime::UlwLoopError::new("body exploded","TEST"))).unwrap_err(); assert!(e.to_string().contains("body exploded")); assert!(with_ulw_loop_mutation_lock(tmp.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>("recovered")).is_ok()); }
#[test]
fn test_lock_second_batch_works() { let tmp=tempfile::tempdir().unwrap(); for _ in 0..3 { with_ulw_loop_mutation_lock(tmp.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); } for _ in 0..2 { with_ulw_loop_mutation_lock(tmp.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); } }
#[test]
fn test_distinct_roots_independent() { let a=tempfile::tempdir().unwrap(); let b=tempfile::tempdir().unwrap(); with_ulw_loop_mutation_lock(a.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); with_ulw_loop_mutation_lock(b.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); }
#[test]
fn test_distinct_scopes_independent() { let tmp=tempfile::tempdir().unwrap(); let a=UlwLoopScope::new("session-a"); with_ulw_loop_mutation_lock(tmp.path(),Some(&a),None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); with_ulw_loop_mutation_lock(tmp.path(),None,None,||Ok::<_,ulw_loop_core::runtime::UlwLoopError>(())).unwrap(); }

#[test]
fn test_minimal_version_one_plan_with_goals_array_is_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(ulw_loop_dir(tmp.path(), None)).unwrap();
    fs::write(ulw_loop_goals_path(tmp.path(), None), "{\"version\":1,\"goals\":[]}").unwrap();
    let plan = read_ulw_loop_plan(tmp.path(), None).unwrap();
    assert!(plan.goals.is_empty());
}

#[test]
fn test_corrupt_or_wrong_shape_goals_json_is_invalid_not_missing() {
    for raw in ["{not json", "{\"version\":1,\"goals\":{}}", "{\"version\":2,\"goals\":[]}", "{\"version\":1}"] {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(ulw_loop_dir(tmp.path(), None)).unwrap();
        fs::write(ulw_loop_goals_path(tmp.path(), None), raw).unwrap();
        let e = read_ulw_loop_plan(tmp.path(), None).unwrap_err();
        assert_eq!(e.code, "ULW_LOOP_PLAN_INVALID", "{raw}");
        assert!(e.message.starts_with("Invalid ulw-loop plan at "), "{}", e.message);
    }
}
