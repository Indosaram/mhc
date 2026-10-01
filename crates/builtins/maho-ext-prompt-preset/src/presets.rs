use crate::settings::{PromptPresetName, parse_prompt_preset};
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
