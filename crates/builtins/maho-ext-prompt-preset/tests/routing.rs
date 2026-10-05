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
fn gpt6_family_aliases_route_to_astra() { for id in ["gpt-6-sol", "gpt-6-luna-fast", "openai/gpt-6-sol", "global.openai.gpt-6-sol", "gpt_6_sol"] { assert_eq!(resolve_preset_name(model(id), P::Auto), Some(P::Gpt6Astra)); } }
#[test]
fn unknown_gpt6_siblings_are_not_routed() { for id in ["gpt-6", "gpt-6-mini", "gpt-6.1", "gpt-6-solaris", "gpt-6-lunar", "solar-pro", "luna-1"] { assert_eq!(resolve_preset_name(model(id), P::Auto), None); } }
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
