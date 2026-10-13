use maho_codemode::{prompt::eval_prompt::{eval_emphasis_style, is_gpt_code_mode_model, EvalEmphasisStyle}, config::settings::Languages, prompt::eval_prompt::{build_eval_prompt, EvalPromptOptions}};

#[test]
fn model_dialect_selection_preserves_family_boundaries() {
    for (model, expected) in [
        (None, EvalEmphasisStyle::Default),
        (Some("gpt-6"), EvalEmphasisStyle::Gpt),
        (Some("provider/GPT.6"), EvalEmphasisStyle::Gpt),
        (Some("claude-sonnet"), EvalEmphasisStyle::Claude),
        (Some("provider@glm5"), EvalEmphasisStyle::Claude),
        (Some("kimi-k2"), EvalEmphasisStyle::Kimi),
        (Some("openai/o3"), EvalEmphasisStyle::Codex),
        (Some("chatgpt-5"), EvalEmphasisStyle::Codex),
        (Some("notgpt-6"), EvalEmphasisStyle::Default),
        (Some("o3 invalid"), EvalEmphasisStyle::Default),
    ] { assert_eq!(eval_emphasis_style(model), expected); }
    assert!(is_gpt_code_mode_model(Some("openai/gpt-6")));
    assert!(!is_gpt_code_mode_model(Some("openai/chatgpt-6")));
}

#[test]
fn disabled_languages_are_rejected() {
    assert!(build_eval_prompt(&Languages {py:false,js:false,rb:false,jl:false}, &EvalPromptOptions::default()).is_err());
}

#[test]
fn every_enabled_language_combination_resolves_template_tags() {
    for mask in 1..16 {
        let enabled = Languages { py:mask & 1 != 0, js:mask & 2 != 0, rb:mask & 4 != 0, jl:mask & 8 != 0 };
        for model in ["unknown", "claude-sonnet", "gpt-6", "codex-6", "kimi-k2"] {
            for spawns in [false, true] {
                let options = EvalPromptOptions { model_id:Some(model.into()), spawns, monitor:true, ..Default::default() };
                let prompt = build_eval_prompt(&enabled, &options).unwrap();
                assert!(!prompt.description.contains("{{"));
                assert!(!prompt.description.contains("}}"));
                assert_eq!(prompt.prompt_guidelines.len(), 2);
            }
        }
    }
}

fn all_languages() -> Languages { Languages { py:true, js:true, rb:true, jl:true } }

#[test]
fn monitor_guidance_appears_only_when_monitor_is_registered() {
    for model in ["unknown", "claude-sonnet", "gpt-6", "codex-6", "kimi-k2"] {
        let without = build_eval_prompt(&all_languages(), &EvalPromptOptions { model_id:Some(model.into()), ..Default::default() }).unwrap();
        assert!(!without.description.contains("tool.monitor"), "{model}");
        let with = build_eval_prompt(&all_languages(), &EvalPromptOptions { model_id:Some(model.into()), monitor:true, ..Default::default() }).unwrap();
        assert!(with.description.contains("tool.monitor({ command, filter })"), "{model}");
    }
}

#[test]
fn dialect_blocks_follow_model_family() {
    let claude = build_eval_prompt(&all_languages(), &EvalPromptOptions { model_id:Some("claude-sonnet".into()), ..Default::default() }).unwrap();
    assert!(claude.description.contains("<eval_first_batching>"));
    let gpt = build_eval_prompt(&all_languages(), &EvalPromptOptions { model_id:Some("gpt-6".into()), ..Default::default() }).unwrap();
    assert!(gpt.description.contains("<gpt_eval_dialect>"));
    assert!(!gpt.description.contains("<eval_first_batching>"));
}

#[test]
fn output_helper_and_spawn_default_agent_require_spawns() {
    let plain = build_eval_prompt(&all_languages(), &EvalPromptOptions::default()).unwrap();
    assert!(!plain.description.contains("output(*ids"));
    let spawning = build_eval_prompt(&all_languages(), &EvalPromptOptions { spawns:true, spawn_default_agent:Some("sisyphus".into()), ..Default::default() }).unwrap();
    assert!(spawning.description.contains("output(*ids"));
    assert!(spawning.description.contains("agent(prompt, agent?=\"sisyphus\""));
}

#[test]
fn language_line_uses_kernel_labels_and_any_joins() {
    let prompt = build_eval_prompt(&Languages { py:false, js:false, rb:true, jl:true }, &EvalPromptOptions::default()).unwrap();
    assert!(prompt.description.contains("`language`: `\"rb\"` persistent Ruby kernel, `\"jl\"` persistent Julia kernel."));
    assert!(prompt.description.contains("Ruby: synchronous; helper options are keyword args;"));
    assert!(prompt.description.contains("Julia: synchronous; helper options are standard keyword args;"));
}
