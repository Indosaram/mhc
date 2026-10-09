use std::path::PathBuf;

use ulw_loop_core::ultrawork_directive::*;
use ulw_loop_core::ultrawork_skill_pointer::*;

#[test]
fn test_ultrawork_skill_pointer_template_contains_placeholders() {
    assert!(ULTRAWORK_SKILL_POINTER_TEMPLATE.contains("<ultrawork-mode>"));
    assert!(ULTRAWORK_SKILL_POINTER_TEMPLATE.contains("ULTRAWORK MODE ENABLED!"));
    assert!(ULTRAWORK_SKILL_POINTER_TEMPLATE.contains("{{ULTRAWORK_SKILL_PATH}}"));
}

#[test]
fn test_build_ultrawork_skill_pointer() {
    let p = PathBuf::from("/path/to/SKILL.md");
    let text = build_ultrawork_skill_pointer(&p);
    assert!(text.contains("/path/to/SKILL.md"));
    assert!(!text.contains("{{ULTRAWORK_SKILL_PATH}}"));
}

#[test]
fn test_build_ultrawork_directive_output_when_ulw_prompt() {
    let input = UltraworkDirectiveInput {
        prompt: "ulw this change",
        transcript_path: None,
    };
    let opts = UltraworkAdditionalContextOptions::default();
    let out = build_ultrawork_directive_output(&input, &opts);
    assert!(!out.is_empty());
    assert!(out.contains("UserPromptSubmit"));
    assert!(out.contains("<ultrawork-mode>"));
}

#[test]
fn test_build_ultrawork_directive_output_when_non_ulw_prompt() {
    let input = UltraworkDirectiveInput {
        prompt: "regular edit",
        transcript_path: None,
    };
    let opts = UltraworkAdditionalContextOptions::default();
    let out = build_ultrawork_directive_output(&input, &opts);
    assert!(out.is_empty());
}
