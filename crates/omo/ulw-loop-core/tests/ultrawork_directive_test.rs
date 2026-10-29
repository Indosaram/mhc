use std::fs;

use ulw_loop_core::codex_hook::{apply_user_prompt_ulw_loop_steering, UserPromptSubmitHookOptions, UserPromptSubmitPayload};
use ulw_loop_core::ultrawork_directive::*;
use ulw_loop_core::ultrawork_skill_pointer::*;

fn payload(prompt: &str, transcript_path: Option<String>) -> UserPromptSubmitPayload {
    UserPromptSubmitPayload {
        cwd: "/tmp".into(),
        hook_event_name: "UserPromptSubmit".into(),
        model: None,
        permission_mode: None,
        prompt: prompt.into(),
        session_id: "s1".into(),
        transcript_path,
        turn_id: None,
    }
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

#[test]
fn test_ulw_prompt_with_existing_transcript_directive_emits_no_duplicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("transcript.jsonl");
    fs::write(
        &transcript,
        r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"<ultrawork-mode>\nexisting directive"}}
"#,
    )
    .expect("write transcript");

    let input = UltraworkDirectiveInput {
        prompt: "ulw this change",
        transcript_path: Some(&transcript),
    };
    let output = build_ultrawork_directive_output(
        &input,
        &UltraworkAdditionalContextOptions::default(),
    );

    assert!(output.is_empty());
}

#[test]
fn test_context_pressure_marker_outside_transcript_tail_does_not_suppress_directive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("transcript.jsonl");
    let padding = "old transcript padding\n".repeat(32_000);
    fs::write(&transcript, format!("context_length_exceeded\n{padding}"))
        .expect("write transcript");

    let input = UltraworkDirectiveInput {
        prompt: "ulw this change",
        transcript_path: Some(&transcript),
    };
    let output = build_ultrawork_directive_output(
        &input,
        &UltraworkAdditionalContextOptions::default(),
    );

    assert!(output.contains("<ultrawork-mode>"));
}

#[test]
fn test_existing_skill_file_emits_compact_skill_pointer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = dir.path().join("SKILL.md");
    fs::write(&skill, "---\nname: ultrawork\n---\n\n<ultrawork-mode>\ndirective body\n</ultrawork-mode>\n")
        .expect("write skill");
    let input = payload("ulw this change", None);
    let options = UserPromptSubmitHookOptions {
        include_ultrawork_directive: true,
        ultrawork_skill_file_path: Some(Some(skill.clone())),
    };

    let output = apply_user_prompt_ulw_loop_steering(&input, &options);
    let parsed: serde_json::Value = serde_json::from_str(&output).expect("hook output JSON");
    let context = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additional context");

    assert_eq!(context, build_ultrawork_skill_pointer(&skill).trim());
}

#[test]
fn test_missing_skill_file_falls_back_to_full_directive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("SKILL.md");
    let input = payload("ulw this change", None);
    let options = UserPromptSubmitHookOptions {
        include_ultrawork_directive: true,
        ultrawork_skill_file_path: Some(Some(missing)),
    };

    let output = apply_user_prompt_ulw_loop_steering(&input, &options);
    let parsed: serde_json::Value = serde_json::from_str(&output).expect("hook output JSON");
    let context = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additional context");

    assert_eq!(context, ULTRAWORK_DIRECTIVE_FALLBACK.trim());
}

#[test]
fn test_ulw_loop_pointer_template_matches_bundled_pointer_contract() {
    let path = std::path::Path::new("/tmp/ultrawork/SKILL.md");
    assert_eq!(
        build_ultrawork_skill_pointer(path),
        ULTRAWORK_SKILL_POINTER_TEMPLATE.replace("{{ULTRAWORK_SKILL_PATH}}", &path.to_string_lossy())
    );
}


