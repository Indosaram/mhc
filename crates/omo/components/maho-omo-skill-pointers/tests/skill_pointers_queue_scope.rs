use crate::support;

use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use support::*;

#[tokio::test]
async fn given_a_queued_prompt_when_streaming_behavior_is_set_then_all_pointers_ride_inside_the_same_message() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "mass ulw loop queued work",
        InputSource::Interactive,
        Some(StreamingBehavior::Steer),
        "test-session",
    ).await;

    let EventResult::Input(InputEventResult::Transform { text, .. }) = result else {
        panic!("expected Transform");
    };
    assert!(text.starts_with("mass ulw loop queued work\n"));
    assert!(text.contains("mass-ulw/SKILL.md"));
    assert!(text.contains("ulw-loop/SKILL.md"));
    assert_eq!(harness.actions.messages.lock().expect("messages").len(), 0);
}

#[tokio::test]
async fn given_ulw_loop_session_scope_when_the_input_session_id_is_known_then_the_pointer_carries_its_normalized_scope() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "run ulw-loop",
        InputSource::Interactive,
        None,
        "session/a weird",
    ).await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: ULW_LOOP_CUSTOM_TYPE, skill_name: "ulw-loop" },
    ]);
    let content = injected_content(&harness, ULW_LOOP_CUSTOM_TYPE);
    assert!(content.contains("`mhc ulw-loop <subcommand> --json`"));
    assert!(content.contains(".omo/ulw-loop/session-a-weird/"));
}

#[tokio::test]
async fn given_ulw_loop_session_scope_when_a_queued_rpc_input_changes_sessions_then_the_pointer_uses_that_events_scope() {
    let harness = setup_harness("/skills/");
    let _ = dispatch_input(
        &harness,
        "run ulw-loop",
        InputSource::Interactive,
        None,
        "previous-session",
    ).await;

    let prompt = "/skill:mass-ulw then ulw-loop";
    let result = dispatch_input(
        &harness,
        prompt,
        InputSource::Rpc,
        Some(StreamingBehavior::Steer),
        "new/session",
    ).await;

    let EventResult::Input(InputEventResult::Transform { text, .. }) = result else {
        panic!("expected Transform");
    };
    assert!(text.starts_with(&format!("{prompt}\n<omo-ulw-loop-pointer>")));
    assert!(text.contains(".omo/ulw-loop/new-session/"));
    assert!(!text.contains(".omo/ulw-loop/previous-session/"));
    assert_eq!(harness.actions.messages.lock().expect("messages").len(), 1);
}

#[tokio::test]
async fn given_ulw_loop_session_scope_when_the_input_session_id_is_unknown_then_the_pointer_omits_the_session_flag() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "run ulw-loop",
        InputSource::Interactive,
        None,
        "",
    ).await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: ULW_LOOP_CUSTOM_TYPE, skill_name: "ulw-loop" },
    ]);
    let content = injected_content(&harness, ULW_LOOP_CUSTOM_TYPE);
    assert!(!content.contains("--session-id"));
    assert!(!content.contains(".omo/ulw-loop/"));
}
