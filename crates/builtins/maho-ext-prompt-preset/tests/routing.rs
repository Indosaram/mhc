use maho_ext_prompt_preset::{presets::{ModelMetadata, resolve_preset_name}, settings::{PromptPresetName as P, load_prompt_preset_settings}};
fn model(id: &str) -> ModelMetadata<'_> { ModelMetadata { id, provider: "custom", name: None, prompt_preset: None } }
#[test]
fn settings_project_override_wins() { assert_eq!(load_prompt_preset_settings(Some("gpt-5.6"), Some("kimi-k3")), P::Gpt56); }
#[test]
fn invalid_project_setting_falls_back_to_global() { assert_eq!(load_prompt_preset_settings(Some("invalid"), Some("kimi-k3")), P::KimiK3); }
#[test]
fn invalid_settings_default_to_auto() { assert_eq!(load_prompt_preset_settings(Some("invalid"), None), P::Auto); }
#[test]
fn explicit_setting_overrides_model_metadata() { let mut model = model("gpt-6-sol"); model.prompt_preset = Some("gpt-5.6"); assert_eq!(resolve_preset_name(model, P::KimiK3), Some(P::KimiK3)); }
#[test]
fn model_metadata_overrides_family() { let mut model = model("gpt-6-sol"); model.prompt_preset = Some("gpt-5.6"); assert_eq!(resolve_preset_name(model, P::Auto), Some(P::Gpt56)); }
#[test]
fn gpt6_family_aliases_route_to_astra() { for id in ["gpt-6-sol", "gpt-6-luna-fast", "openai/gpt-6-sol", "global.openai.gpt-6-sol", "gpt_6_sol", "gpt-6.1-sol", "gpt-6.1-sol-fast", "openai/gpt-6.1-sol", "openai-gpt-61-sol"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::Gpt6Astra)); } }
#[test]
fn unknown_gpt6_siblings_are_not_routed() { for id in ["gpt-6", "gpt-6-mini", "gpt-6.1", "gpt-61", "gpt-6-solaris", "gpt-6-lunar", "solar-pro", "luna-1"] { assert_eq!(resolve_preset_name(model(id), P::Auto), None); } }
#[test]
fn display_name_routes_family_when_id_has_no_signal() { let mut model = model("default"); model.name = Some("GPT-6 Sol"); assert_eq!(resolve_preset_name(model, P::Auto), Some(P::Gpt6Astra)); }
#[test]
fn official_retired_deepseek_alias_routes_to_v41() { let mut model = model("deepseek-v4-flash"); model.provider = "deepseek"; assert_eq!(resolve_preset_name(model, P::Auto), Some(P::DeepseekV41Flash)); }
#[test]
fn other_provider_preserves_deepseek_v4() { assert_eq!(resolve_preset_name(model("deepseek-v4-flash"), P::Auto), Some(P::DeepseekV4Flash)); }
#[test]
fn dated_deepseek_snapshot_precedes_retired_alias() { let mut model = model("deepseek-v4-flash-0731"); model.provider = "deepseek"; assert_eq!(resolve_preset_name(model, P::Auto), Some(P::DeepseekV4Flash0731)); }
#[test]
fn specific_claude_release_precedes_generic_family() { assert_eq!(resolve_preset_name(model("claude-opus-5.5"), P::Auto), Some(P::ClaudeOpus55)); assert_eq!(resolve_preset_name(model("claude-mythos-5.1"), P::Auto), Some(P::ClaudeFable51)); }
#[test]
fn rolling_kimi_product_aliases_route_by_version() { assert_eq!(resolve_preset_name(model("kimi-for-coding"), P::Auto), Some(P::KimiK28)); assert_eq!(resolve_preset_name(model("kimi-for-coding-highspeed"), P::Auto), Some(P::KimiK27)); }
#[test]
fn swe2_served_lanes_route_to_kimi_k3() { let devin = |id| ModelMetadata { id, provider: "devin", name: None, prompt_preset: None }; for id in ["swe-2-medium", "swe-2-high", "swe-2-max", "devin/swe-2-medium", "devin/swe-2-high"] { assert_eq!(resolve_preset_name(devin(id), P::Auto), Some(P::KimiK3), "model id {id}"); } for id in ["swe-2", "swe-2-low", "swe-2-high-lite"] { assert_ne!(resolve_preset_name(devin(id), P::Auto), Some(P::KimiK3), "unserved model id {id} (senpi#2306)"); } }
#[test]
fn kimi_k3_1_ids_route_before_k3() { for id in ["k3.1-pro", "k3.1-pro-highspeed", "kimi-k3.1-pro", "kimi-k3.1-pro-highspeed", "kimi-k3-1-pro", "k3p1", "kimi-for-coding/k3p1"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::KimiK31), "model id {id}"); } assert_eq!(resolve_preset_name(ModelMetadata { id: "x", provider: "custom", name: Some("Kimi K3.1"), prompt_preset: None }, P::Auto), Some(P::KimiK31)); assert_eq!(resolve_preset_name(model("kimi-k3"), P::Auto), Some(P::KimiK3)); }
#[test]
fn unserved_swe2_lanes_are_not_routed() { for id in ["swe-2", "devin/swe-2", "devin/swe-2-low", "devin/swe-2-high-lite"] { assert_eq!(resolve_preset_name(model(id), P::Auto), None, "model id {id}"); } }
#[test]
fn upstream_prompt_preset_model_cases_resolve() {
 for (provider, id, preset) in [
  ("openai", "gpt-6-astra", P::Gpt6Astra), ("openai", "gpt-5.6", P::Gpt56), ("openai", "gpt-5.5", P::Gpt55), ("openai", "gpt-5.4", P::Gpt54),
  ("openai", "gpt-5.3-codex", P::Gpt53Codex), ("openai", "gpt-5.2", P::Gpt52), ("kimi-coding", "k3.1-pro", P::KimiK31), ("moonshotai", "kimi-k3", P::KimiK3),
  ("kimi-for-coding", "kimi-for-coding", P::KimiK28), ("kimi-for-coding", "kimi-for-coding-highspeed", P::KimiK27), ("moonshotai", "kimi-k2.6", P::KimiK26),
  ("zai-coding-plan", "glm-5.3", P::Glm53), ("zai-coding-plan", "glm-5.2", P::Glm52), ("deepseek", "deepseek-v4-flash-0731", P::DeepseekV4Flash0731),
  ("deepseek", "deepseek-flash", P::DeepseekV41Flash), ("fireworks", "accounts/fireworks/models/deepseek-v4-flash", P::DeepseekV4Flash), ("deepseek", "deepseek-v4-pro", P::DeepseekV4Pro),
  ("anthropic", "claude-fable-5", P::ClaudeFable5), ("anthropic", "claude-fable-5.1", P::ClaudeFable51), ("anthropic", "claude-fable-5-5", P::ClaudeFable55),
  ("anthropic", "claude-opus-5.5", P::ClaudeOpus55), ("anthropic", "claude-opus-5", P::ClaudeOpus5), ("anthropic", "claude-sonnet-5.5", P::ClaudeSonnet55),
  ("anthropic", "claude-haiku-5.5", P::ClaudeHaiku55), ("anthropic", "claude-opus-4-8", P::ClaudeOpus48), ("anthropic", "claude-opus-4-7", P::ClaudeOpus47),
  ("anthropic", "claude-opus-4-6", P::ClaudeOpus46), ("anthropic", "claude-opus-4-5", P::ClaudeOpus45), ("xai", "grok-4.7", P::Grok47), ("xai", "grok-4.6", P::Grok46),
  ("xai", "grok-4.5", P::Grok45), ("google", "gemini-4-argon", P::Gemini4Argon),
 ] {
  assert_eq!(resolve_preset_name(ModelMetadata { id, provider, name: None, prompt_preset: None }, P::Auto), Some(preset), "{provider}/{id}");
 }
}
#[test]
fn claude_55_gateway_spellings_route_to_tuned_cores() {
 for id in ["anthropic.claude-sonnet-5-5", "anthropic/claude-sonnet-5.5:batch", "claude-sonnet-5-5-high"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::ClaudeSonnet55), "model id {id}"); }
 for id in ["us.anthropic.claude-haiku-5-5", "anthropic/claude-haiku-5.5-batch"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::ClaudeHaiku55), "model id {id}"); }
}
#[test]
fn gemini_4_argon_variants_route_and_bare_gemini_4_does_not() {
 for id in ["gemini-4-argon-high", "google/gemini-4-argon:batch", "gemini-4-argon-contributor"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::Gemini4Argon), "model id {id}"); }
 assert_eq!(resolve_preset_name(model("gemini-4"), P::Auto), None);
}
