use crate::support;

use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use support::*;

#[tokio::test]
async fn delegated_roles_suppress_skill_pointers_while_root_sessions_inject() {
    for role in ["child", "dag_child", "member", "future-role"] {
        let harness = setup_harness_with_role("/skills/", Some(role));
        let result = dispatch_input(
            &harness,
            "ulw research the market",
            InputSource::Interactive,
            None,
            "session",
        )
        .await;
        expect_no_injection(&harness, &result);
    }
    let harness = setup_harness_with_role("/skills/", Some(""));
    let result = dispatch_input(&harness, "ulw-loop", InputSource::Interactive, None, "root").await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: ULW_LOOP_CUSTOM_TYPE,
            skill_name: "ulw-loop",
        }],
    );
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_a_mention_is_inside_inline_code_then_no_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    for text in [
        "explain `ulw-loop` without running it",
        "explain ``ulw-loop `status` ``",
    ] {
        let empty: Vec<&str> = Vec::new();
        assert_eq!(matched_skill_pointer_names(text), empty);
        let result = dispatch_input(
            &harness,
            text,
            InputSource::Interactive,
            None,
            "test-session",
        )
        .await;
        expect_no_injection(&harness, &result);
    }
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_a_mention_is_inside_a_fenced_code_block_then_no_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    for text in [
        "review this log:\n```text\nulw-loop\n```",
        "~~~text\nulw-loop\n~~~",
        "````text\n```\nulw-loop\n````",
        "```text\nulw-loop",
    ] {
        let empty: Vec<&str> = Vec::new();
        assert_eq!(matched_skill_pointer_names(text), empty);
        let result = dispatch_input(
            &harness,
            text,
            InputSource::Interactive,
            None,
            "test-session",
        )
        .await;
        expect_no_injection(&harness, &result);
    }
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_a_mention_is_inside_a_relayed_pointer_block_then_no_pointer_is_injected()
 {
    let harness = setup_harness("/skills/");
    for tag in [
        "omo-ulw-loop-pointer",
        "ultrawork-mode",
        "omo-ultrawork-reminder",
    ] {
        let text = format!("<{tag}>ulw-loop</{tag}>");
        let empty: Vec<&str> = Vec::new();
        assert_eq!(matched_skill_pointer_names(&text), empty);
        let result = dispatch_input(
            &harness,
            &text,
            InputSource::Interactive,
            None,
            "test-session",
        )
        .await;
        expect_no_injection(&harness, &result);
    }
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_code_separates_skill_name_fragments_then_removing_it_does_not_fabricate_a_match()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "ulw `not a request` loop",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_a_request_follows_a_quoted_mention_then_only_the_requested_skill_matches()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "explain `ulw-plan`, then run ulw-loop",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: ULW_LOOP_CUSTOM_TYPE,
            skill_name: "ulw-loop",
        }],
    );
}

#[tokio::test]
async fn given_quoted_and_relayed_mentions_when_plain_prose_mentions_a_skill_then_the_pointer_remains_harmlessly_conditional()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "the status report mentions ulw-loop",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: ULW_LOOP_CUSTOM_TYPE,
            skill_name: "ulw-loop",
        }],
    );
    let content = injected_content(&harness, ULW_LOOP_CUSTOM_TYPE);
    assert!(content.ends_with("</omo-ulw-loop-pointer>"));
}

#[tokio::test]
async fn given_suppression_conditions_when_the_source_is_extension_then_nothing_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "ulw loop from extension",
        InputSource::Extension,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_suppression_conditions_when_the_prompt_is_a_raw_skill_command_for_the_only_matched_skill_then_nothing_is_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "/skill:ulw-loop run the graph",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_suppression_conditions_when_skill_ulw_loop_args_mention_ulw_research_then_the_research_pointer_and_its_companion_are_injected_not_ulw_loops()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "/skill:ulw-loop then ulw research the fallout",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[
            ExpectedPointer {
                custom_type: ULW_RESEARCH_CUSTOM_TYPE,
                skill_name: "ulw-research",
            },
            ExpectedPointer {
                custom_type: BROWSER_CUSTOM_TYPE,
                skill_name: "browser",
            },
            ExpectedPointer {
                custom_type: ULTIMATE_BROWSING_CUSTOM_TYPE,
                skill_name: "ultimate-browsing",
            },
        ],
    );
}

#[tokio::test]
async fn given_suppression_conditions_when_the_prompt_carries_an_expanded_skill_block_then_that_skill_is_not_re_injected()
 {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        r#"<skill name="ulw-loop" path="skills/ulw-loop/SKILL.md">skill body mentioning ulw loop</skill> now run it"#,
        InputSource::Interactive,
        None,
        "test-session",
    ).await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_suppression_conditions_when_the_component_flag_is_disabled_then_nothing_is_injected()
{
    let harness = setup_harness("/skills/");
    harness
        .api
        .runtime
        .set_flag(SKILL_POINTERS_DISABLED_FLAG, FlagValue::Boolean(true));
    let result = dispatch_input(
        &harness,
        "ulw loop ship it",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_suppression_conditions_when_the_text_has_no_keyword_then_nothing_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(
        &harness,
        "ordinary follow-up",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_no_injection(&harness, &result);
}

#[tokio::test]
async fn given_a_real_skills_root_when_a_matched_skill_is_not_shipped_then_its_pointer_is_withheld()
{
    let root = std::env::temp_dir().join(format!("skill-pointers-shipped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ulw-plan")).expect("create skill dir");
    std::fs::write(
        root.join("ulw-plan").join("SKILL.md"),
        "---\nname: ulw-plan\n---\n",
    )
    .expect("write SKILL.md");
    let root_str = format!("{}/", root.display());
    assert!(skill_is_shipped(&root_str, "ulw-plan"));
    assert!(!skill_is_shipped(&root_str, "ulw-loop"));
    assert!(skill_is_shipped(
        "/definitely/not/a/skills/root/",
        "ulw-loop"
    ));

    let harness = setup_harness(&root_str);
    let result = dispatch_input(
        &harness,
        "ulw loop and ulw plan it out",
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: ULW_PLAN_CUSTOM_TYPE,
            skill_name: "ulw-plan",
        }],
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn strip_quoted_spans_preserves_byte_length_for_multibyte_text() {
    for text in [
        "한글 설명\n```\ncode\n```\nulw-plan",
        "가나 `코드` \"인용\"\n~~~\n다라\n~~~\n```\n마바\n```\nulw-loop",
    ] {
        let visible = strip_quoted_spans(text);
        assert_eq!(visible.len(), text.len(), "{text}");
        assert!(
            visible.ends_with(&text[text.rfind('\n').expect("newline")..]),
            "{text}"
        );
    }
}

#[test]
fn large_prompts_are_stripped_without_panicking_on_the_backtrack_budget() {
    for text in [
        "가".repeat(240_000),
        "a".repeat(700_000),
        "`".repeat(700_000),
        "'".repeat(700_000),
        "don't use ".repeat(70_000),
    ] {
        let visible = strip_quoted_regions(&text);
        assert_eq!(visible.len(), text.len());
        assert_eq!(matched_skill_pointer_names(&text), Vec::<&str>::new());
    }
}

#[test]
fn large_prompt_ending_with_a_real_request_still_matches() {
    for filler in ["가".repeat(240_000), "a".repeat(700_000)] {
        let text = format!("{filler}\nrun ulw loop");
        assert_eq!(matched_skill_pointer_names(&text), vec!["ulw-loop"]);
    }
}

#[tokio::test]
async fn given_a_large_prompt_when_it_ends_with_a_real_request_then_the_pointer_is_injected() {
    let harness = setup_harness("/skills/");
    let text = format!("{}\nrun ulw loop", "가".repeat(240_000));
    let result = dispatch_input(
        &harness,
        &text,
        InputSource::Interactive,
        None,
        "test-session",
    )
    .await;
    expect_pointer_injections(
        &harness,
        &result,
        &[ExpectedPointer {
            custom_type: ULW_LOOP_CUSTOM_TYPE,
            skill_name: "ulw-loop",
        }],
    );
}
