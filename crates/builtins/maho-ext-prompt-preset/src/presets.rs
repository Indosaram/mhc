use crate::settings::{PromptPresetName, parse_prompt_preset};
use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
pub struct ResolvedPromptPreset {
    pub name: PromptPresetName,
    pub prompt: String,
    pub parts: Vec<maho_ext_api::DynamicSystemPromptPart>,
}
pub fn resolve_preset(
    model: ModelMetadata<'_>,
    setting: PromptPresetName,
    mut options: BuildDynamicSystemPromptOptions<'_>,
) -> Option<ResolvedPromptPreset> {
    let name = resolve_preset_name(model, setting)?;
    options.tuning_section = None;
    options.core_prompt = None;
    options.workstation_dialect = None;
    let captured = std::cell::RefCell::new(Vec::new());
    let on_parts = options.on_parts;
    let observer = |parts: &[maho_ext_api::DynamicSystemPromptPart]| {
        *captured.borrow_mut() = parts.to_vec();
        if let Some(on_parts) = on_parts {
            on_parts(parts);
        }
    };
    let options = BuildDynamicSystemPromptOptions {
        on_parts: Some(&observer),
        ..options
    };
    let prompt = match name {
        PromptPresetName::Auto => unreachable!("auto presets do not render"),
        PromptPresetName::ClaudeFable5 => {
            crate::claude_fable_5::build_claude_fable5_prompt(options)
        }
        PromptPresetName::ClaudeFable55 => {
            crate::claude_fable_5_5::build_claude_fable55_prompt(options)
        }
        PromptPresetName::ClaudeFable51 => {
            crate::claude_fable_5_1::build_claude_fable51_prompt(options)
        }
        PromptPresetName::ClaudeOpus55 => {
            crate::claude_opus_5_5::build_claude_opus55_prompt(options)
        }
        PromptPresetName::ClaudeSonnet55 => {
            crate::claude_sonnet_5_5::build_claude_sonnet55_prompt(options)
        }
        PromptPresetName::ClaudeHaiku55 => {
            crate::claude_haiku_5_5::build_claude_haiku55_prompt(options)
        }
        PromptPresetName::Gemini4Argon => {
            crate::gemini_4_argon::build_gemini4_argon_prompt(options)
        }
        PromptPresetName::ClaudeOpus5 => crate::claude_opus_5::build_claude_opus5_prompt(options),
        PromptPresetName::ClaudeOpus48 => {
            crate::claude_opus_4_8::build_claude_opus48_prompt(options)
        }
        PromptPresetName::ClaudeOpus47 => {
            crate::claude_opus_4_7::build_claude_opus47_prompt(options)
        }
        PromptPresetName::ClaudeOpus46 => {
            crate::claude_opus_4_6::build_claude_opus46_prompt(options)
        }
        PromptPresetName::ClaudeOpus45 => {
            crate::claude_opus_4_5::build_claude_opus45_prompt(options)
        }
        PromptPresetName::DeepseekV4Flash => {
            crate::deepseek_v4_flash::build_deepseek_v4_flash_prompt(options)
        }
        PromptPresetName::DeepseekV4Flash0731 => {
            crate::deepseek_v4_flash_0731::build_deepseek_v4_flash_0731_prompt(options)
        }
        PromptPresetName::DeepseekV41Flash => {
            crate::deepseek_v4_1_flash::build_deepseek_v41_flash_prompt(options)
        }
        PromptPresetName::DeepseekV4Pro => {
            crate::deepseek_v4_pro::build_deepseek_v4_pro_prompt(options)
        }
        PromptPresetName::Glm52 => crate::glm_5_2::build_glm52_prompt(options),
        PromptPresetName::Glm53 => crate::glm_5_3::build_glm53_prompt(options),
        PromptPresetName::Grok45 => crate::grok_4_5::build_grok45_prompt(options),
        PromptPresetName::Grok46 => crate::grok_4_6::build_grok46_prompt(options),
        PromptPresetName::Grok47 => crate::grok_4_7::build_grok47_prompt(options),
        PromptPresetName::KimiK31 => crate::kimi_k3_1::build_kimi_k3_1_prompt(options),
        PromptPresetName::KimiK3 => crate::kimi_k3::build_kimi_k3_prompt(options),
        PromptPresetName::KimiK28 => crate::kimi_k2_8::build_kimi_k28_prompt(options),
        PromptPresetName::KimiK27 => crate::kimi_k2_7::build_kimi_k27_prompt(options),
        PromptPresetName::KimiK26 => crate::kimi_k2_6::build_kimi_k26_prompt(options),
        PromptPresetName::Gpt5 => crate::gpt_5::build_gpt5_prompt(options),
        PromptPresetName::Gpt52 => crate::gpt_5_2::build_gpt52_prompt(options),
        PromptPresetName::Gpt53Codex => crate::gpt_5_3_codex::build_gpt53_codex_prompt(options),
        PromptPresetName::Gpt54 => crate::gpt_5_4::build_gpt54_prompt(options),
        PromptPresetName::Gpt55 => crate::gpt_5_5::build_gpt55_prompt(options),
        PromptPresetName::Gpt56 => crate::gpt_5_6::build_gpt56_prompt(options),
        PromptPresetName::Gpt6Astra => crate::gpt_6_astra::build_gpt6_astra_prompt(options),
    };
    let mut parts = captured.into_inner();
    for part in &mut parts {
        if part.name != "project_context"
            || part
                .source
                .as_deref()
                .is_none_or(|source| source == "dynamic-prompt")
        {
            part.source = Some(format!("prompt-preset:{}", name.as_str()));
        }
    }
    Some(ResolvedPromptPreset {
        name,
        prompt,
        parts,
    })
}
#[derive(Clone, Copy)]
pub struct ModelMetadata<'a> {
    pub id: &'a str,
    pub provider: &'a str,
    pub name: Option<&'a str>,
    pub prompt_preset: Option<&'a str>,
}
pub fn resolve_preset_name(
    model: ModelMetadata<'_>,
    setting: PromptPresetName,
) -> Option<PromptPresetName> {
    if setting != PromptPresetName::Auto {
        return Some(setting);
    }
    if let Some(preset) =
        parse_prompt_preset(model.prompt_preset).filter(|preset| *preset != PromptPresetName::Auto)
    {
        return Some(preset);
    }
    // senpi presets.ts: auto resolution is the shared contract, so the two never drift.
    crate::contract::resolve_preset_name(crate::contract::ContractModel {
        provider_id: model.provider,
        model_id: model.id,
        name: model.name,
    })
}
