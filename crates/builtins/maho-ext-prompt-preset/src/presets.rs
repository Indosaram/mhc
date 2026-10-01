use crate::settings::{PromptPresetName, parse_prompt_preset};
use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
pub struct ResolvedPromptPreset { pub name: PromptPresetName, pub prompt: String }
pub fn resolve_preset(model: ModelMetadata<'_>, setting: PromptPresetName, options: BuildDynamicSystemPromptOptions<'_>) -> Option<ResolvedPromptPreset> {
 let name = resolve_preset_name(model, setting)?;
 let prompt = match name {
  PromptPresetName::Auto => return None,
  PromptPresetName::ClaudeFable5 => crate::claude_fable_5::build_claude_fable5_prompt(options),
  PromptPresetName::ClaudeFable51 => crate::claude_fable_5_1::build_claude_fable51_prompt(options),
  PromptPresetName::ClaudeOpus55 => crate::claude_opus_5_5::build_claude_opus55_prompt(options),
  PromptPresetName::ClaudeOpus5 => crate::claude_opus_5::build_claude_opus5_prompt(options),
  PromptPresetName::ClaudeOpus48 => crate::claude_opus_4_8::build_claude_opus48_prompt(options),
  PromptPresetName::ClaudeOpus47 => crate::claude_opus_4_7::build_claude_opus47_prompt(options),
  PromptPresetName::ClaudeOpus46 => crate::claude_opus_4_6::build_claude_opus46_prompt(options),
  PromptPresetName::ClaudeOpus45 => crate::claude_opus_4_5::build_claude_opus45_prompt(options),
  PromptPresetName::DeepseekV4Flash => crate::deepseek_v4_flash::build_deepseek_v4_flash_prompt(options),
  PromptPresetName::DeepseekV4Flash0731 => crate::deepseek_v4_flash_0731::build_deepseek_v4_flash_0731_prompt(options),
  PromptPresetName::DeepseekV41Flash => crate::deepseek_v4_1_flash::build_deepseek_v41_flash_prompt(options),
  PromptPresetName::DeepseekV4Pro => crate::deepseek_v4_pro::build_deepseek_v4_pro_prompt(options),
  PromptPresetName::Glm52 => crate::glm_5_2::build_glm52_prompt(options),
  PromptPresetName::Glm53 => crate::glm_5_3::build_glm53_prompt(options),
  PromptPresetName::Grok45 => crate::grok_4_5::build_grok45_prompt(options),
  PromptPresetName::Grok46 => crate::grok_4_6::build_grok46_prompt(options),
  PromptPresetName::Grok47 => crate::grok_4_7::build_grok47_prompt(options),
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
 Some(ResolvedPromptPreset { name, prompt })
}
#[derive(Clone, Copy)]
pub struct ModelMetadata<'a> { pub id: &'a str, pub provider: &'a str, pub name: Option<&'a str>, pub prompt_preset: Option<&'a str> }
fn normalized(value: &str) -> String { regex::Regex::new(r"\s+").map_or_else(|_| value.to_lowercase(), |pattern| pattern.replace_all(&value.to_lowercase(), "-").into_owned()) }
fn signal(value: &str, patterns: &[&str]) -> bool { let value = normalized(value); patterns.iter().any(|pattern| regex::Regex::new(pattern).is_ok_and(|pattern| pattern.is_match(&value))) }
fn model_signal(model: ModelMetadata<'_>, patterns: &[&str]) -> bool { signal(model.id, patterns) || model.name.is_some_and(|name| signal(name, patterns)) }
pub fn resolve_preset_name(model: ModelMetadata<'_>, setting: PromptPresetName) -> Option<PromptPresetName> {
 if setting != PromptPresetName::Auto { return Some(setting); }
 if let Some(preset) = parse_prompt_preset(model.prompt_preset).filter(|preset| *preset != PromptPresetName::Auto) { return Some(preset); }
 if model_signal(model, &[r"(?:^|[/@:._-])gpt[._-]?6[._-](?:astra|sol|luna)(?:$|[/@:._-])"]) { return Some(PromptPresetName::Gpt6Astra); }
 let id = normalized(model.id);
 if id.contains("gpt-5.6") { return Some(PromptPresetName::Gpt56); }
 if id.contains("gpt-5.5") { return Some(PromptPresetName::Gpt55); }
 if id.contains("gpt-5.4") { return Some(PromptPresetName::Gpt54); }
 if id.contains("gpt-5.3") { return Some(PromptPresetName::Gpt53Codex); }
 if id.contains("gpt-5.2") { return Some(PromptPresetName::Gpt52); }
 if model_signal(model, &[r"(?:^|[/@:._-])swe-2-(?:high|max|low|high-lite)(?:$|[/@:._-])"]) || model_signal(model, &[r"(?:^|[/@._-])kimi-k3(?:$|[/@._:-])"]) || id == "k3" || model.name.is_some_and(|name| normalized(name) == "k3") { return Some(PromptPresetName::KimiK3); }
 if model_signal(model, &[r"(?:^|[/@._-])kimi-k2(?:[._-]|p)8(?:$|[/@._:-])"]) || id == "kimi-for-coding" || model.name.is_some_and(|name| normalized(name) == "kimi-for-coding") { return Some(PromptPresetName::KimiK28); }
 if model_signal(model, &[r"(?:^|[/@._-])kimi-k2(?:[._-]|p)7(?:$|[/@._:-])"]) || id == "kimi-for-coding-highspeed" || model.name.is_some_and(|name| normalized(name) == "kimi-for-coding-highspeed") { return Some(PromptPresetName::KimiK27); }
 if model_signal(model, &[r"(?:^|[/@._-])kimi-k2(?:[._-]|p)6(?:$|[/@._:-])"]) { return Some(PromptPresetName::KimiK26); }
 if ["fable-5-1","fable-5.1","mythos-5-1","mythos-5.1"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeFable51); }
 if ["fable-5","mythos-5"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeFable5); }
 if ["opus-5-5","opus-5.5"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus55); }
 if ["opus-5"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus5); }
 if ["opus-4-8"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus48); }
 if ["opus-4-7"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus47); }
 if ["opus-4-6"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus46); }
 if ["opus-4-5","opus-4.5"].iter().any(|marker| id.contains(marker)) { return Some(PromptPresetName::ClaudeOpus45); }
 if model_signal(model, &[r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)3(?:$|[/@._:-])"]) { return Some(PromptPresetName::Glm53); }
 if model_signal(model, &[r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)2(?:$|[/@._:-])"]) { return Some(PromptPresetName::Glm52); }
 if model_signal(model, &[r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash[._-]0731(?:$|[/@:._-])"]) { return Some(PromptPresetName::DeepseekV4Flash0731); }
 if model_signal(model, &[r"(?:^|[/@:._-])deepseek[._-]v4(?:[._-]1|p1)[._-]flash(?:$|[/@:._-])", r"(?:^|[/@:._-])deepseek[._-]flash(?:$|[/@:._-])"]) || (model.provider == "deepseek" && signal(model.id, &[r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash(?:$|[/@:._-])"])) { return Some(PromptPresetName::DeepseekV41Flash); }
 if model_signal(model, &[r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash(?:$|[/@:._-])"]) { return Some(PromptPresetName::DeepseekV4Flash); }
 if model_signal(model, &[r"(?:^|[/@:._-])deepseek[._-]v4[._-]pro(?:$|[/@:._-])"]) { return Some(PromptPresetName::DeepseekV4Pro); }
 if model_signal(model, &[r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?7(?:$|[/@._:-])"]) { return Some(PromptPresetName::Grok47); }
 if model_signal(model, &[r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?6(?:$|[/@._:-])"]) { return Some(PromptPresetName::Grok46); }
 if model_signal(model, &[r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?5(?:$|[/@._:-])"]) { return Some(PromptPresetName::Grok45); }
 None
}
