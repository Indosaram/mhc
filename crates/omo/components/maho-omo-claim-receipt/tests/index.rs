//! Port of `claim-receipt/index.test.ts` (the JS sidecar loader cases are out of scope: the native
//! component links the evaluator directly, so there is no import that can fail).

use crate::support::{
    answer, draft_types, edit_result, passing_test, user_input, Harness,
};
use maho_ext_api::InputSource;
use maho_omo_claim_receipt::verdict::{GATE_FLAGS_TAG, NUDGE_MESSAGE_TYPE, VERDICT_ENTRY_TYPE};
use serde_json::{json, Value};

fn unsupported() -> Vec<Value> {
    let mut entries = vec![user_input("fix")];
    entries.extend(edit_result("e1", "src/a.ts"));
    entries.push(answer("All tests pass."));
    entries
}

fn supported() -> Vec<Value> {
    let mut entries = vec![user_input("fix")];
    entries.extend(edit_result("e1", "src/a.ts"));
    entries.extend(passing_test("t1"));
    entries.push(answer("All tests pass."));
    entries
}

#[tokio::test]
async fn a_final_answer_with_an_unsupported_claim_appends_a_verdict_record_and_one_visible_nudge() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    let settle = harness.finish_run(unsupported(), None, None).await;
    assert!(!settle.should_continue, "the nudge never continues the run");
    let entries = settle.entries.expect("drafts");
    assert_eq!(
        draft_types(&entries),
        vec!["other", VERDICT_ENTRY_TYPE, NUDGE_MESSAGE_TYPE]
    );
    assert_eq!(entries[2]["display"], json!(true));
    assert_eq!(entries[1]["data"]["flagged"], json!(true));
    assert_eq!(entries[1]["data"]["nudged"], json!(true));
    assert_eq!(entries[1]["data"]["version"], json!(1));
    assert_eq!(
        entries[1]["data"]["verdicts"][0]["status"],
        json!("unsupported")
    );
    assert_eq!(entries[1]["data"]["verdicts"][0]["reason"], json!("no-check"));
}

#[tokio::test]
async fn repeated_answers_in_one_human_input_epoch_nudge_once_and_rearm_only_on_user_input() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    assert_eq!(harness.finish(unsupported()).await.map(|entries| entries.len()), Some(3));
    assert_eq!(
        harness.finish(unsupported()).await.map(|entries| draft_types(&entries)),
        Some(vec!["other".to_owned(), VERDICT_ENTRY_TYPE.to_owned()])
    );
    harness.input(InputSource::Extension).await;
    assert_eq!(harness.finish(unsupported()).await.map(|entries| entries.len()), Some(2));
    harness.input(InputSource::Rpc).await;
    assert_eq!(harness.finish(unsupported()).await.map(|entries| entries.len()), Some(3));
}

#[tokio::test]
async fn a_supported_answer_records_a_supported_verdict_without_a_nudge() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    let entries = harness.finish(supported()).await.expect("drafts");
    assert_eq!(
        draft_types(&entries),
        vec!["other", VERDICT_ENTRY_TYPE]
    );
    assert_eq!(entries[1]["data"]["flagged"], json!(false));
    assert_eq!(entries[1]["data"]["nudged"], json!(false));
}

#[tokio::test]
async fn a_claim_free_answer_adds_nothing() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    let entries = vec![user_input("hi"), answer("Here is the summary you asked for.")];
    assert!(harness.finish(entries).await.is_none());
}

#[tokio::test]
async fn each_answer_records_the_epoch_it_ended_in() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    let first = harness.finish(unsupported()).await.expect("drafts");
    assert_eq!(first[1]["data"]["epoch"], json!(1));
    harness.input(InputSource::Interactive).await;
    let second = harness.finish(unsupported()).await.expect("drafts");
    assert_eq!(second[1]["data"]["epoch"], json!(2));
    assert_eq!(second[1]["data"]["nudged"], json!(true));
}

#[tokio::test]
async fn aborted_retried_and_non_completed_runs_and_memory_worker_children_stay_out() {
    let harness = Harness::new(&[], true);
    assert!(harness.finish_run(unsupported(), Some(true), None).await.entries.is_none());
    assert!(harness.finish_run(unsupported(), None, Some(true)).await.entries.is_none());
    let child = Harness::new(&[("SENPI_MEMORY_FACTS", "1")], true);
    assert!(child.finish(unsupported()).await.is_none());
}

#[tokio::test]
async fn the_default_shadow_mode_records_the_flag_but_shows_no_nudge_and_leaves_gate_prompts_alone() {
    let harness = Harness::new(&[], false);
    harness.input(InputSource::Interactive).await;
    let entries = harness.finish(unsupported()).await.expect("drafts");
    assert_eq!(draft_types(&entries), vec!["other", VERDICT_ENTRY_TYPE]);
    assert_eq!(entries[1]["data"]["flagged"], json!(true));
    assert_eq!(entries[1]["data"]["nudged"], json!(false));
    assert_eq!(
        entries[1]["data"]["verdicts"][0]["status"],
        json!("unsupported")
    );
    let gate = harness
        .tool_call(json!({
            "subagent_type": "omo-native-gate-reviewer",
            "prompt": "Review it."
        }))
        .await;
    assert_eq!(gate["prompt"], json!("Review it."));
}

#[tokio::test]
async fn a_gate_reviewer_spawn_carries_the_flags_once_and_other_spawns_are_untouched() {
    let harness = Harness::new(&[], true);
    harness.input(InputSource::Interactive).await;
    harness.finish(unsupported()).await;

    let gate = json!({ "subagent_type": "omo-native-gate-reviewer", "prompt": "Review it." });
    let gate = harness.tool_call(gate).await;
    let gate = harness.tool_call(gate).await;
    let tag = format!("<{GATE_FLAGS_TAG}>");
    let prompt = gate["prompt"].as_str().expect("prompt");
    assert_eq!(prompt.matches(tag.as_str()).count(), 1);
    assert!(prompt.contains("All tests pass."));

    let batch = harness
        .tool_call(json!({
            "subagent_type": "omo-native-gate-reviewer",
            "tasks": [{ "prompt": "A" }, { "subagent_type": "explore", "prompt": "B" }]
        }))
        .await;
    assert!(
        batch["tasks"][0]["prompt"]
            .as_str()
            .expect("task prompt")
            .contains(tag.as_str())
    );
    assert_eq!(batch["tasks"][1]["prompt"], json!("B"));

    let explore = harness
        .tool_call(json!({ "subagent_type": "explore", "prompt": "Look." }))
        .await;
    assert_eq!(explore["prompt"], json!("Look."));

    harness.input(InputSource::Interactive).await;
    let later = harness
        .tool_call(json!({ "subagent_type": "omo-native-gate-reviewer", "prompt": "Again." }))
        .await;
    assert_eq!(later["prompt"], json!("Again."));
}
