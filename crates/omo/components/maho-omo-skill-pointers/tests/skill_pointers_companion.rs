use crate::support;

use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use support::*;

const RESEARCH: ExpectedPointer = ExpectedPointer { custom_type: ULW_RESEARCH_CUSTOM_TYPE, skill_name: "ulw-research" };
const BROWSER: ExpectedPointer = ExpectedPointer { custom_type: BROWSER_CUSTOM_TYPE, skill_name: "browser" };
const COMPANION: ExpectedPointer = ExpectedPointer { custom_type: ULTIMATE_BROWSING_CUSTOM_TYPE, skill_name: "ultimate-browsing" };

#[tokio::test]
async fn loaded_browser_is_not_injected_again_for_research() {
    for text in [
        r#"<skill name="browser" path="skills/browser/SKILL.md">engines</skill> ulw research the gateway options"#,
        "/skill:browser then ulw research the fallout",
    ] {
        let harness = setup_harness("/skills/");
        let result = dispatch_input(&harness, text, InputSource::Interactive, None, "session").await;
        expect_pointer_injections(&harness, &result, &[RESEARCH, COMPANION]);
    }
}

#[tokio::test]
async fn given_a_ulw_research_invocation_when_research_is_mentioned_then_the_ultimate_browsing_companion_rides_behind_the_research_pointer() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "ulw research the gateway options", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[RESEARCH, BROWSER, COMPANION]);
    let companion = injected_content(&harness, ULTIMATE_BROWSING_CUSTOM_TYPE);
    assert!(companion.contains("ulw-research"));
    assert!(companion.contains(r#"load_skills: ["ultimate-browsing"]"#));
}

#[tokio::test]
async fn given_a_ulw_research_invocation_when_the_prompt_is_a_raw_skill_ulw_research_command_then_only_the_companion_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "/skill:ulw-research the gateway options", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[BROWSER, COMPANION]);
}

#[tokio::test]
async fn given_a_ulw_research_invocation_when_the_prompt_carries_an_expanded_ulw_research_block_then_only_the_companion_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        r#"<skill name="ulw-research" path="skills/ulw-research/SKILL.md">skill body mentioning ulw research</skill> now run it"#,
        InputSource::Interactive,
        None,
        "test-session",
    ).await;

    expect_pointer_injections(&harness, &result, &[BROWSER, COMPANION]);
}

#[tokio::test]
async fn given_a_ulw_research_invocation_when_mass_ulw_research_is_mentioned_then_the_companion_is_injected_once_after_every_keyword_pointer() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "mass ulw research the gateway options, then ulw research it again", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: MASS_ULW_CUSTOM_TYPE, skill_name: "mass-ulw" },
        RESEARCH,
        BROWSER,
        COMPANION,
    ]);
}

#[tokio::test]
async fn given_a_ulw_research_invocation_when_the_prompt_is_queued_mid_stream_then_both_pointers_ride_inside_the_transformed_text() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "ulw research the gateway options", InputSource::Interactive, Some(StreamingBehavior::FollowUp), "test-session").await;

    assert_eq!(harness.actions.messages.lock().expect("messages").len(), 0);
    let EventResult::Input(InputEventResult::Transform { text, .. }) = result else {
        panic!("expected Transform result");
    };
    assert!(text.starts_with("ulw research the gateway options\n"));
    let research_idx = text.find("<omo-ulw-research-pointer>").expect("research pointer");
    let browsing_idx = text.find("<omo-ultimate-browsing-pointer>").expect("browsing pointer");
    let browser_idx = text.find("<omo-browser-pointer>").expect("browser pointer");
    assert!(research_idx < browser_idx);
    assert!(browser_idx < browsing_idx);
    assert!(text.contains("ultimate-browsing/SKILL.md"));
}

#[tokio::test]
async fn given_the_companion_is_already_loaded_when_the_prompt_carries_an_expanded_ultimate_browsing_block_then_the_companion_is_not_re_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        r#"<skill name="ultimate-browsing" path="skills/ultimate-browsing/SKILL.md">tiers</skill> ulw research the gateway options"#,
        InputSource::Interactive,
        None,
        "test-session",
    ).await;

    expect_pointer_injections(&harness, &result, &[RESEARCH, BROWSER]);
}

#[tokio::test]
async fn given_the_companion_is_already_loaded_when_the_prompt_is_a_raw_skill_ultimate_browsing_command_mentioning_ulw_research_then_the_companion_is_not_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "/skill:ultimate-browsing then ulw research the fallout", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[RESEARCH, BROWSER]);
}

#[tokio::test]
async fn given_no_ulw_research_invocation_when_ultimate_browsing_is_mentioned_on_its_own_then_nothing_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "use ultimate-browsing to screenshot the page", InputSource::Interactive, None, "test-session").await;

    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_no_ulw_research_invocation_when_only_ulw_loop_is_mentioned_then_no_companion_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "ulw loop ship the refactor", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: ULW_LOOP_CUSTOM_TYPE, skill_name: "ulw-loop" },
    ]);
}
