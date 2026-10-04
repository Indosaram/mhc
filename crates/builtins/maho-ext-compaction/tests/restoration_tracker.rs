use maho_ext_compaction::restoration_tracker::*;
use serde_json::{Value,json};
fn options(settings:&RestorationSettings)->PreparePendingPayloadOptions<'_> {PreparePendingPayloadOptions {accepted:true,reason:"manual",compaction_entry_id:"compact",context_window:200000.,usage_tokens:Some(10000.),reserve_tokens:16384.,settings,kept_messages:&[]}}
#[test]
fn fractional_limits_are_floored_only_by_restoration_consumer() {
    let fractional = RestorationSettings {max_items:Some(2.5),max_tokens_per_item:Some(11.5),max_total_tokens:Some(22.5),..Default::default()};
    let integral = RestorationSettings {max_items:Some(2.),max_tokens_per_item:Some(11.),max_total_tokens:Some(22.),..Default::default()};
    let mut state = RestorationTrackerState::default();
    for name in ["a", "b", "c"] {track_tool_call(&mut state,"skill",&json!({"name":name}));}
    let mut floored = state.clone();
    assert_eq!(compute_restoration_budget(&options(&fractional)),22.);
    prepare_pending_payload(&mut state,&options(&fractional));
    prepare_pending_payload(&mut floored,&options(&integral));
    assert_eq!(consume_pending_payload(&mut state),consume_pending_payload(&mut floored));
}
#[test]
fn equal_budget_labels_follow_default_locale_collation() {
    let mut state = RestorationTrackerState::default();
    for name in ["z", "A", "a", "é", "e"] { track_tool_call(&mut state, "skill", &json!({"name":name})); }
    prepare_pending_payload(&mut state, &options(&Default::default()));
    let payload = consume_pending_payload(&mut state).unwrap();
    let labels: Vec<_> = payload["details"]["items"].as_array().unwrap().iter().map(|item| item["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["a", "A", "e", "é", "z"]);
}
#[test] fn pending_payload_is_consumed_once_after_acceptance() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"read",&json!({"path":"src/a.rs"}));track_tool_call(&mut state,"edit",&json!({"path":"src/b.rs"}));track_tool_call(&mut state,"skill",&json!({"name":"rust"}));prepare_pending_payload(&mut state,&options(&Default::default()));assert!(state.pending_payload.is_some());let payload=consume_pending_payload(&mut state).expect("payload");assert_eq!(payload["details"]["items"].as_array().expect("items").len(),3);assert!(consume_pending_payload(&mut state).is_none());}
#[test] fn budget_is_minimum_of_config_ratio_and_headroom() {let settings=RestorationSettings {max_total_tokens:Some(7000.),context_ratio:Some(0.2),..Default::default()};let mut input=options(&settings);input.context_window=50000.;input.usage_tokens=Some(30000.);input.reserve_tokens=16000.;assert_eq!(compute_restoration_budget(&input),4000.);}
#[test] fn retained_labels_are_not_restored() {let mut state=RestorationTrackerState::default();for path in ["kept.rs","lost.rs"] {track_tool_call(&mut state,"read",&json!({"path":path}));}let settings=Default::default();let kept=[json!({"role":"user","content":"kept.rs"})];let mut input=options(&settings);input.kept_messages=&kept;prepare_pending_payload(&mut state,&input);let payload=consume_pending_payload(&mut state).expect("payload");assert_eq!(payload["details"]["items"][0]["label"],"lost.rs");}
#[test] fn oversized_item_is_truncated_to_limit() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"skill",&json!({"name":"x".repeat(400)}));let settings=RestorationSettings {max_tokens_per_item:Some(20.),..Default::default()};prepare_pending_payload(&mut state,&options(&settings));let payload=consume_pending_payload(&mut state).expect("payload");assert!(payload["details"]["items"][0]["tokens"].as_u64().expect("tokens")<=20);assert!(payload["details"]["items"][0]["content"].as_str().expect("content").contains("[... truncated]"));}
#[test] fn restored_labels_stay_suppressed_when_new_items_arrive() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"read",&json!({"path":"first.rs"}));prepare_pending_payload(&mut state,&options(&Default::default()));consume_pending_payload(&mut state);track_tool_call(&mut state,"edit",&json!({"path":"second.rs"}));prepare_pending_payload(&mut state,&options(&Default::default()));let p=consume_pending_payload(&mut state).expect("payload");assert_eq!(p["details"]["items"].as_array().expect("items").len(),1);assert_eq!(p["details"]["items"][0]["label"],"second.rs");}
#[test] fn files_and_skills_use_plural_sections() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"read",&json!({"path":"file.rs"}));track_tool_call(&mut state,"skill",&json!({"name":"rust"}));prepare_pending_payload(&mut state,&options(&Default::default()));let p=consume_pending_payload(&mut state).expect("payload");let content=p["content"].as_str().expect("content");assert!(content.contains("<restored-files>"));assert!(content.contains("<restored-skills>"));}
#[test] fn item_cap_selects_highest_priority() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"read",&json!({"path":"read.rs"}));track_tool_call(&mut state,"edit",&json!({"path":"edited.rs"}));track_tool_call(&mut state,"skill",&json!({"name":"rust"}));let settings=RestorationSettings {max_items:Some(2.),..Default::default()};prepare_pending_payload(&mut state,&options(&settings));let p=consume_pending_payload(&mut state).expect("payload");assert_eq!(p["details"]["items"][0]["label"],"edited.rs");assert_eq!(p["details"]["items"][1]["label"],"rust");}
#[test] fn rejected_compaction_does_not_replace_pending_payload() {let mut state=RestorationTrackerState {pending_payload:Some(json!({"existing":true})),..Default::default()};let settings=Default::default();let mut input=options(&settings);input.accepted=false;prepare_pending_payload(&mut state,&input);assert_eq!(state.pending_payload,Some(json!({"existing":true})));}
#[test] fn patch_paths_and_operations_are_merged() {let mut state=RestorationTrackerState::default();track_tool_call(&mut state,"read",&json!({"path":"a.rs"}));track_tool_call(&mut state,"apply_patch",&json!({"input":"*** Begin Patch\n*** Update File: a.rs\n@@\n-old\n+new\n*** End Patch"}));assert_eq!(state.items["a.rs"].content,"a.rs (read, edit)");assert_eq!(state.items["a.rs"].priority,100);}
#[test] fn zero_budget_clears_pending_payload() {let mut state=RestorationTrackerState {pending_payload:Some(Value::Null),..Default::default()};let settings=RestorationSettings {max_total_tokens:Some(0.),..Default::default()};prepare_pending_payload(&mut state,&options(&settings));assert!(state.pending_payload.is_none());}
