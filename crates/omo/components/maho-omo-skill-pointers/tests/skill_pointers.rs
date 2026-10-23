use crate::support;

use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use support::*;

#[test]
fn given_the_keyword_table_when_given_mass_ulw_trigger_spellings_then_mass_ulw_matches() {
    let triggers = [
        "mass ulw",
        "massulw",
        "MASS ULW",
        "Mass-Ulw",
        "mass  ulw",
        "run mass ulw now",
        "mass-ulw",
        "ulw mass",
        "ulwmass",
        "mulw",
        "meth",
    ];
    for text in triggers {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, vec!["mass-ulw"], "trigger '{text}' failed");
    }
}

#[test]
fn given_the_keyword_table_when_given_ulw_plan_trigger_spellings_then_ulw_plan_matches() {
    let triggers = [
        "ulw plan",
        "ulw-plan",
        "ulwplan",
        "ULW PLAN",
        "Ulw-Plan",
        "go ulw plan the migration",
    ];
    for text in triggers {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, vec!["ulw-plan"], "trigger '{text}' failed");
    }
}

#[test]
fn given_the_keyword_table_when_given_ulw_loop_trigger_spellings_then_ulw_loop_matches() {
    let triggers = [
        "ulw loop",
        "ulw-loop",
        "ulwloop",
        "ULW LOOP",
        "ulw  loop",
        "go ulw loop",
    ];
    for text in triggers {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, vec!["ulw-loop"], "trigger '{text}' failed");
    }
}

#[test]
fn given_the_keyword_table_when_given_ulw_research_trigger_spellings_then_ulw_research_matches() {
    let triggers = [
        "ulw research",
        "ulw-research",
        "ulwresearch",
        "ULW RESEARCH",
    ];
    for text in triggers {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, vec!["ulw-research"], "trigger '{text}' failed");
    }
}

#[test]
fn given_the_keyword_table_when_keywords_overlap_then_every_mentioned_skill_matches_in_table_order() {
    let cases = [
        ("mass ulw loop", vec!["mass-ulw", "ulw-loop"]),
        ("mass ulw-loop", vec!["mass-ulw", "ulw-loop"]),
        ("mass ulw research", vec!["mass-ulw", "ulw-research"]),
        ("mulw research", vec!["mass-ulw", "ulw-research"]),
        ("meth research", vec!["mass-ulw", "ulw-research"]),
        ("ulw mass research", vec!["mass-ulw", "ulw-research"]),
        ("ulwmass-research", vec!["mass-ulw", "ulw-research"]),
        ("MULW RESEARCH", vec!["mass-ulw", "ulw-research"]),
        ("mass ulw plan it out", vec!["mass-ulw", "ulw-plan"]),
        ("ulw loop then ulw research", vec!["ulw-loop", "ulw-research"]),
        ("ulw research first, ulw loop second", vec!["ulw-loop", "ulw-research"]),
        ("make pr work until gets merged go ulw loop", vec!["ulw-loop"]),
    ];
    for (text, expected) in cases {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, expected, "case '{text}' failed");
    }
}

#[test]
fn given_the_keyword_table_when_given_near_miss_spellings_then_nothing_matches() {
    let misses = [
        "ulw",
        "ultrawork",
        "the mass of ulw",
        "ulw massive",
        "ulwmassive",
        "simulw",
        "mulwark",
        "method",
        "methods",
        "methane",
        "promethean",
        "amethyst",
        "ulw-looper",
        "ulwloops go brr",
        "ulw planning session",
        "loop ulw",
        "research ulw",
        "kulw loop of yarn",
        "just loop it",
    ];
    for text in misses {
        let matched = matched_skill_pointer_names(text);
        let empty: Vec<&str> = Vec::new();
        assert_eq!(matched, empty, "near-miss '{text}' should not match");
    }
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_the_user_requests_a_skill_then_one_hidden_conditional_pointer_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "mass ulw ship the docs refresh", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[ExpectedPointer {
        custom_type: MASS_ULW_CUSTOM_TYPE,
        skill_name: "mass-ulw",
    }]);

    let content = injected_content(&harness, MASS_ULW_CUSTOM_TYPE);
    assert!(content.ends_with("</omo-mass-ulw-pointer>"));
    assert!(content.contains("If the user of this session is asking to run mass-ulw"));
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_the_mass_ulw_pointer_is_injected_then_it_points_at_the_packaged_skill() {
    let harness = setup_harness("/skills/");
    let _ = dispatch_input(&harness, "mass ulw ship it", InputSource::Interactive, None, "test-session").await;

    let content = injected_content(&harness, MASS_ULW_CUSTOM_TYPE);
    assert!(content.contains("mass-ulw/SKILL.md"));
    assert!(content.ends_with("</omo-mass-ulw-pointer>"));
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_ulw_loop_and_mass_ulw_pointers_are_injected_then_only_ulw_loop_teaches_the_native_cli() {
    let harness = setup_harness("/skills/");
    let _ = dispatch_input(&harness, "mass ulw-loop ship the refactor", InputSource::Interactive, None, "test-session").await;

    let mass_content = injected_content(&harness, MASS_ULW_CUSTOM_TYPE);
    let loop_content = injected_content(&harness, ULW_LOOP_CUSTOM_TYPE);

    assert!(loop_content.contains("`mhc ulw-loop <subcommand> --json`"));
    assert!(!loop_content.contains("OMO_AGENT_TOOLKIT_SDK_ROOT"));
    assert!(!loop_content.contains("tool.omo_agent_toolkit"));
    assert!(!loop_content.contains("runtime/agent-toolkit"));
    assert!(!mass_content.contains("mhc ulw-loop"));
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_overlapping_keywords_are_mentioned_then_one_pointer_per_skill_is_injected_in_table_order() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "mass ulw-loop ship the refactor", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: MASS_ULW_CUSTOM_TYPE, skill_name: "mass-ulw" },
        ExpectedPointer { custom_type: ULW_LOOP_CUSTOM_TYPE, skill_name: "ulw-loop" },
    ]);
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_ulw_plan_is_mentioned_then_the_plan_pointer_is_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "go ulw plan the migration", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: ULW_PLAN_CUSTOM_TYPE, skill_name: "ulw-plan" },
    ]);
}

#[tokio::test]
async fn given_a_matching_interactive_prompt_when_research_is_mentioned_then_the_research_pointer_and_its_browsing_companion_are_injected() {
    let harness = setup_harness("/skills/");
    let result = dispatch_input(&harness, "mass ulw research the gateway options", InputSource::Interactive, None, "test-session").await;

    expect_pointer_injections(&harness, &result, &[
        ExpectedPointer { custom_type: MASS_ULW_CUSTOM_TYPE, skill_name: "mass-ulw" },
        ExpectedPointer { custom_type: ULW_RESEARCH_CUSTOM_TYPE, skill_name: "ulw-research" },
        ExpectedPointer { custom_type: BROWSER_CUSTOM_TYPE, skill_name: "browser" },
        ExpectedPointer { custom_type: ULTIMATE_BROWSING_CUSTOM_TYPE, skill_name: "ultimate-browsing" },
    ]);
}

#[test]
fn given_unicode_adjacency_when_non_ascii_chars_border_keyword_then_ascii_word_boundaries_match() {
    // In JavaScript RegExp, \b is defined strictly on ASCII [0-9A-Za-z_].
    // Non-ASCII Unicode characters (Korean, CJK, etc.) count as non-word chars,
    // so borders between Unicode characters and ASCII keywords match \b.
    let cases = [
        ("한ulw-loop", vec!["ulw-loop"]),
        ("ulw-loop한", vec!["ulw-loop"]),
        ("한ulw-loop한", vec!["ulw-loop"]),
        ("실행ulw-loop시작", vec!["ulw-loop"]),
        ("한mass-ulw", vec!["mass-ulw"]),
        ("テストulw-plan", vec!["ulw-plan"]),
        ("한ulw-research", vec!["ulw-research"]),
    ];
    for (text, expected) in cases {
        let matched = matched_skill_pointer_names(text);
        assert_eq!(matched, expected, "Unicode adjacency case '{text}' failed");
    }
}
