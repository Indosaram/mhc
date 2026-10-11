#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Write;
use serde_json::{json, Value};
use ulw_loop_core::stop_resume_hook::run_stop_resume_hook;

fn setup() -> tempfile::TempDir { tempfile::tempdir().unwrap() }
fn payload(dir: &tempfile::TempDir, overrides: Value) -> Value { let mut base=json!({"hook_event_name":"Stop","session_id":"s1","turn_id":"t1","transcript_path":dir.path().join("transcript.jsonl"),"cwd":dir.path(),"model":"gpt-5.6-sol","permission_mode":"default","stop_hook_active":false}); base.as_object_mut().unwrap().extend(overrides.as_object().unwrap().clone()); base }
fn goal(id: &str, status: &str) -> Value { json!({"id":id,"title":format!("Goal {id}"),"objective":format!("Objective {id}"),"status":status,"successCriteria":[],"attempt":1,"createdAt":"2026-07-11T00:00:00.000Z","updatedAt":"2026-07-11T00:00:00.000Z"}) }
fn write_goals(dir: &tempfile::TempDir, goals: Vec<Value>) { let state=dir.path().join(".omo/ulw-loop/s1"); fs::create_dir_all(&state).unwrap(); fs::write(state.join("goals.json"), serde_json::to_vec(&json!({"version":1,"createdAt":"2026-07-11T00:00:00.000Z","updatedAt":"2026-07-11T00:00:00.000Z","briefPath":".omo/ulw-loop/s1/brief.md","goalsPath":".omo/ulw-loop/s1/goals.json","ledgerPath":".omo/ulw-loop/s1/ledger.jsonl","codexGoalMode":"aggregate","goals":goals})).unwrap()).unwrap(); fs::write(state.join("ledger.jsonl"), "").unwrap(); fs::write(dir.path().join("transcript.jsonl"), "").unwrap(); }
fn boulder(dir: &tempfile::TempDir, plan: &str, flat: bool) { fs::create_dir_all(dir.path().join(".omo/plans")).unwrap(); fs::write(dir.path().join(".omo/plans/p.md"), plan).unwrap(); let v=if flat {json!({"session_ids":["codex:s1"],"status":"active","active_plan":".omo/plans/p.md"})} else {json!({"works":{"w1":{"session_ids":["codex:s1"],"status":"active","active_plan":".omo/plans/p.md"}}})}; fs::write(dir.path().join(".omo/boulder.json"),serde_json::to_vec(&v).unwrap()).unwrap(); }

#[test] fn pending_goal_blocks_with_resume_directive(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);let o=run_stop_resume_hook(&payload(&d,json!({})));let v:Value=serde_json::from_str(&o).unwrap();assert_eq!(v["decision"],"block");assert!(v["reason"].as_str().unwrap().contains("mhc ulw-loop status"));}
#[test] fn active_stop_hook_noops(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);assert_eq!(run_stop_resume_hook(&payload(&d,json!({"stop_hook_active":true}))),"");}
#[test] fn malformed_payload_noops(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);assert_eq!(run_stop_resume_hook(&json!({"hook_event_name":"Stop"})),"");}
#[test] fn active_boulder_with_remaining_tasks_defers(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);boulder(&d,"## TODOs\n- [ ] pending task\n",false);assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn exhausted_boulder_plan_defers(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);boulder(&d,"## TODOs\n- [x] done task\n",false);assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn flat_legacy_boulder_defers(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);boulder(&d,"## TODOs\n- [ ] pending task\n",true);assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn context_pressure_noops(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);fs::write(d.path().join("transcript.jsonl"),"note: context compacted mid-run\n").unwrap();assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn no_state_noops(){let d=setup();assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn complete_goals_noop(){let d=setup();write_goals(&d,vec![goal("g1","complete"),goal("g2","complete")]);assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn blocked_and_failed_goals_noop(){let d=setup();write_goals(&d,vec![goal("g1","blocked"),goal("g2","failed")]);assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");}
#[test] fn static_ledger_caps_and_marks_stuck(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);let a=run_stop_resume_hook(&payload(&d,json!({})));let b=run_stop_resume_hook(&payload(&d,json!({})));let c=run_stop_resume_hook(&payload(&d,json!({})));assert_eq!(serde_json::from_str::<Value>(&a).unwrap()["decision"],"block");assert_eq!(serde_json::from_str::<Value>(&b).unwrap()["decision"],"block");assert_eq!(c,"");let state=d.path().join(".omo/ulw-loop/s1");assert_eq!(serde_json::from_slice::<Value>(&fs::read(state.join("auto-resume-g1.json")).unwrap()).unwrap()["count"],2);assert!(state.join("auto-resume-g1.stuck").exists());}
#[test] fn session_id_normalizes_in_directive(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);let v:Value=serde_json::from_str(&run_stop_resume_hook(&payload(&d,json!({"session_id":"s1/"})))).unwrap();assert!(v["reason"].as_str().unwrap().contains("--session-id s1"));}
#[test] fn traversal_goal_id_writes_nothing_outside_state(){let d=setup();write_goals(&d,vec![goal("../../../escaped-marker","pending")]);let outside=d.path().join(".omo/ulw-loop");fs::write(outside.join("escaped-marker.json"),"sentinel\n").unwrap();assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");assert_eq!(fs::read_to_string(outside.join("escaped-marker.json")).unwrap(),"sentinel\n");assert!(!outside.join("escaped-marker.stuck").exists());}
#[test] fn normal_goal_writes_counter_and_stuck_inside_state(){let d=setup();write_goals(&d,vec![goal("goal-abc","pending")]);run_stop_resume_hook(&payload(&d,json!({})));run_stop_resume_hook(&payload(&d,json!({})));assert_eq!(run_stop_resume_hook(&payload(&d,json!({}))),"");let state=d.path().join(".omo/ulw-loop/s1");assert_eq!(serde_json::from_slice::<Value>(&fs::read(state.join("auto-resume-goal-abc.json")).unwrap()).unwrap()["count"],2);assert_eq!(fs::read_to_string(state.join("auto-resume-goal-abc.stuck")).unwrap(),"no ledger progress after 2 resumes\n");}
#[test] fn ledger_movement_resets_cap(){let d=setup();write_goals(&d,vec![goal("g1","pending")]);run_stop_resume_hook(&payload(&d,json!({})));run_stop_resume_hook(&payload(&d,json!({})));fs::OpenOptions::new().append(true).open(d.path().join(".omo/ulw-loop/s1/ledger.jsonl")).unwrap().write_all(b"{\"at\":\"2026-07-11T00:00:00Z\",\"kind\":\"goal_started\"}\n").unwrap();let v:Value=serde_json::from_str(&run_stop_resume_hook(&payload(&d,json!({})))).unwrap();assert_eq!(v["decision"],"block");assert!(!d.path().join(".omo/ulw-loop/s1/auto-resume-g1.stuck").exists());}

#[test]
fn existing_non_stop_payload_is_ignored() {
    assert_eq!(run_stop_resume_hook(&json!({"hook_event_name":"UserPromptSubmit","session_id":"s1","cwd":"/tmp","transcript_path":"/tmp/t.jsonl","stop_hook_active":false})), "");
}

#[test]
fn existing_active_stop_payload_is_ignored() {
    assert_eq!(run_stop_resume_hook(&json!({"hook_event_name":"Stop","session_id":"s1","cwd":"/tmp","transcript_path":"/tmp/t.jsonl","stop_hook_active":true})), "");
}

