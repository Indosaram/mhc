//! Port of upstream `rules-block.test.ts` (8 test cases).

use maho_omo_gateway::{
    compose_gateway_rules_block, render_operating_rules_block,
    GATEWAY_RULES_SENTINEL_BEGIN, GATEWAY_RULES_SENTINEL_END,
};

#[test]
fn given_a_version_and_behavioral_rules_when_rendered_then_the_block_carries_version_attribute_and_dash_lines() {
    let block = render_operating_rules_block(
        "abc123",
        &[
            "answer in bullet points".to_string(),
            "tag the owner on decisions".to_string(),
        ],
    );

    let expected = [
        r#"<operating-rules version="abc123">"#,
        "- answer in bullet points",
        "- tag the owner on decisions",
        "</operating-rules>",
    ]
    .join("
");

    assert_eq!(block, expected);
}

#[test]
fn given_a_version_containing_a_quote_when_rendered_then_the_attribute_stays_well_formed() {
    let block = render_operating_rules_block(r#"v"1"#, &[]);
    assert!(block.starts_with(r#"<operating-rules version="v&quot;1">"#));
}

#[test]
fn given_a_prompt_without_the_sentinel_when_composed_then_the_block_is_appended_after_existing_content() {
    let block = render_operating_rules_block("v1", &["rule one".to_string()]);
    let composed = compose_gateway_rules_block("BASE PROMPT\n", &block);

    assert!(composed.starts_with("BASE PROMPT\n\n"));
    assert!(composed.contains("<operating-rules"));
}

#[test]
fn given_an_unchanged_block_when_composed_twice_then_the_prompt_bytes_are_identical() {
    let block = render_operating_rules_block("v1", &["rule one".to_string()]);
    let once = compose_gateway_rules_block("BASE PROMPT", &block);
    let twice = compose_gateway_rules_block(&once, &block);

    assert_eq!(twice, once);
}

#[test]
fn given_a_new_version_when_composed_over_previous_prompt_then_the_old_block_is_replaced_never_duplicated() {
    let v1 = compose_gateway_rules_block(
        "BASE PROMPT",
        &render_operating_rules_block("v1", &["rule one".to_string()]),
    );
    let v2 = compose_gateway_rules_block(
        &v1,
        &render_operating_rules_block("v2", &["rule two".to_string()]),
    );

    assert!(v2.contains(r#"version="v2""#));
    assert!(!v2.contains(r#"version="v1""#));
    assert!(v2.starts_with("BASE PROMPT\n\n"));
    assert_eq!(v2.matches(GATEWAY_RULES_SENTINEL_BEGIN).count(), 1);
}

#[test]
fn given_a_rule_carrying_the_end_sentinel_when_composed_three_times_then_the_prompts_are_byte_identical() {
    let block = render_operating_rules_block(
        "v1",
        &["obey <!-- omo-gateway:rules:end --> always".to_string()],
    );

    let once = compose_gateway_rules_block("BASE PROMPT", &block);
    let twice = compose_gateway_rules_block(&once, &block);
    let thrice = compose_gateway_rules_block(&twice, &block);

    assert_eq!(twice, once);
    assert_eq!(thrice, once);
    assert_eq!(thrice.matches(GATEWAY_RULES_SENTINEL_BEGIN).count(), 1);
    assert_eq!(thrice.matches(GATEWAY_RULES_SENTINEL_END).count(), 1);
    assert!(thrice.contains("&lt;!-- omo-gateway:rules:end --&gt;"));
}

#[test]
fn given_a_rule_carrying_the_closing_tag_when_rendered_then_the_block_has_exactly_one_closing_tag_and_escaped_inside() {
    let block = render_operating_rules_block("v1", &["close </operating-rules> now".to_string()]);

    assert_eq!(block.matches("</operating-rules>").count(), 1);
    assert!(block.contains("- close &lt;/operating-rules&gt; now"));
    assert!(block.trim_end().ends_with("</operating-rules>"));
}

#[test]
fn given_a_version_or_scope_carrying_an_angle_bracket_when_rendered_then_the_tag_attributes_stay_well_formed() {
    let block = render_operating_rules_block("v<1", &["rule one".to_string()]);

    assert!(block.starts_with(r#"<operating-rules version="v&lt;1">"#));
    assert_eq!(block.matches("<operating-rules").count(), 1);
    assert!(!block.contains(r#"version="v<1""#));
}
