#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PromptPresetName {
 #[default]
 Auto,
 ClaudeFable5,
 ClaudeFable51,
 ClaudeFable55,
 ClaudeOpus55,
 ClaudeOpus5,
 ClaudeSonnet55,
 ClaudeHaiku55,
 ClaudeOpus48,
 ClaudeOpus47,
 ClaudeOpus46,
 ClaudeOpus45,
 DeepseekV4Flash,
 DeepseekV4Flash0731,
 DeepseekV41Flash,
 DeepseekV4Pro,
 Glm52,
 Glm53,
 Gemini4Argon,
 Grok45,
 Grok46,
 Grok47,
 KimiK3,
 KimiK28,
 KimiK27,
 KimiK26,
 Gpt5,
 Gpt52,
 Gpt53Codex,
 Gpt54,
 Gpt55,
 Gpt56,
 Gpt6Astra,
}
impl PromptPresetName {
 pub const fn as_str(self) -> &'static str { match self {
 Self::Auto => "auto",
 Self::ClaudeFable5 => "claude-fable-5",
 Self::ClaudeFable55 => "claude-fable-5-5",
 Self::ClaudeFable51 => "claude-fable-5-1",
 Self::ClaudeOpus55 => "claude-opus-5-5",
 Self::ClaudeOpus5 => "claude-opus-5",
 Self::ClaudeSonnet55 => "claude-sonnet-5-5",
 Self::ClaudeHaiku55 => "claude-haiku-5-5",
 Self::ClaudeOpus48 => "claude-opus-4-8",
 Self::ClaudeOpus47 => "claude-opus-4-7",
 Self::ClaudeOpus46 => "claude-opus-4-6",
 Self::ClaudeOpus45 => "claude-opus-4-5",
 Self::DeepseekV4Flash => "deepseek-v4-flash",
 Self::DeepseekV4Flash0731 => "deepseek-v4-flash-0731",
 Self::DeepseekV41Flash => "deepseek-v4-1-flash",
 Self::DeepseekV4Pro => "deepseek-v4-pro",
 Self::Glm52 => "glm-5.2",
 Self::Glm53 => "glm-5.3",
 Self::Gemini4Argon => "gemini-4-argon",
 Self::Grok45 => "grok-4.5",
 Self::Grok46 => "grok-4.6",
 Self::Grok47 => "grok-4.7",
 Self::KimiK3 => "kimi-k3",
 Self::KimiK28 => "kimi-k2-8",
 Self::KimiK27 => "kimi-k2-7",
 Self::KimiK26 => "kimi-k2-6",
 Self::Gpt5 => "gpt-5",
 Self::Gpt52 => "gpt-5.2",
 Self::Gpt53Codex => "gpt-5.3-codex",
 Self::Gpt54 => "gpt-5.4",
 Self::Gpt55 => "gpt-5.5",
 Self::Gpt56 => "gpt-5.6",
 Self::Gpt6Astra => "gpt-6-astra",
 } }
}
pub fn parse_prompt_preset(value: Option<&str>) -> Option<PromptPresetName> {
 match value? {
"auto" => Some(PromptPresetName::Auto),
"claude-fable-5" => Some(PromptPresetName::ClaudeFable5),
"claude-fable-5-5" => Some(PromptPresetName::ClaudeFable55),
"claude-fable-5-1" => Some(PromptPresetName::ClaudeFable51),
"claude-opus-5-5" => Some(PromptPresetName::ClaudeOpus55),
"claude-opus-5" => Some(PromptPresetName::ClaudeOpus5),
"claude-sonnet-5-5" => Some(PromptPresetName::ClaudeSonnet55),
"claude-haiku-5-5" => Some(PromptPresetName::ClaudeHaiku55),
"claude-opus-4-8" => Some(PromptPresetName::ClaudeOpus48),
"claude-opus-4-7" => Some(PromptPresetName::ClaudeOpus47),
"claude-opus-4-6" => Some(PromptPresetName::ClaudeOpus46),
"claude-opus-4-5" => Some(PromptPresetName::ClaudeOpus45),
"deepseek-v4-flash" => Some(PromptPresetName::DeepseekV4Flash),
"deepseek-v4-flash-0731" => Some(PromptPresetName::DeepseekV4Flash0731),
"deepseek-v4-1-flash" => Some(PromptPresetName::DeepseekV41Flash),
"deepseek-v4-pro" => Some(PromptPresetName::DeepseekV4Pro),
"glm-5.2" => Some(PromptPresetName::Glm52),
"glm-5.3" => Some(PromptPresetName::Glm53),
"gemini-4-argon" => Some(PromptPresetName::Gemini4Argon),
"grok-4.5" => Some(PromptPresetName::Grok45),
"grok-4.6" => Some(PromptPresetName::Grok46),
"grok-4.7" => Some(PromptPresetName::Grok47),
"kimi-k3" => Some(PromptPresetName::KimiK3),
"kimi-k2-8" => Some(PromptPresetName::KimiK28),
"kimi-k2-7" => Some(PromptPresetName::KimiK27),
"kimi-k2-6" => Some(PromptPresetName::KimiK26),
"gpt-5" => Some(PromptPresetName::Gpt5),
"gpt-5.2" => Some(PromptPresetName::Gpt52),
"gpt-5.3-codex" => Some(PromptPresetName::Gpt53Codex),
"gpt-5.4" => Some(PromptPresetName::Gpt54),
"gpt-5.5" => Some(PromptPresetName::Gpt55),
"gpt-5.6" => Some(PromptPresetName::Gpt56),
"gpt-6-astra" => Some(PromptPresetName::Gpt6Astra),
 _ => None,
 }
}
pub fn load_prompt_preset_settings(project: Option<&str>, global: Option<&str>) -> PromptPresetName { parse_prompt_preset(project).or_else(|| parse_prompt_preset(global)).unwrap_or_default() }

pub const VALID_PRESETS: &[&str] = &[
    "auto", "claude-fable-5-5", "claude-fable-5", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5",
    "claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-4-8", "claude-opus-4-7",
    "claude-opus-4-6", "claude-opus-4-5", "deepseek-v4-flash", "deepseek-v4-flash-0731",
    "deepseek-v4-1-flash", "deepseek-v4-pro", "glm-5.2", "glm-5.3", "gemini-4-argon",
    "grok-4.5", "grok-4.6", "grok-4.7", "kimi-k3", "kimi-k2-8", "kimi-k2-7", "kimi-k2-6",
    "gpt-5", "gpt-5.2", "gpt-5.3-codex", "gpt-5.4", "gpt-5.5", "gpt-5.6", "gpt-6-astra",
];

pub fn is_valid_preset(value: &str) -> bool { VALID_PRESETS.contains(&value) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_upstream_preset_values_are_valid_and_parseable() {
        for value in VALID_PRESETS {
            assert!(is_valid_preset(value));
            assert!(parse_prompt_preset(Some(value)).is_some(), "missing preset {value}");
        }
        assert!(!is_valid_preset("not-a-preset"));
        assert_eq!(PromptPresetName::ClaudeSonnet55.as_str(), "claude-sonnet-5-5");
        assert_eq!(PromptPresetName::ClaudeHaiku55.as_str(), "claude-haiku-5-5");
        assert_eq!(PromptPresetName::Gemini4Argon.as_str(), "gemini-4-argon");
    }
}
