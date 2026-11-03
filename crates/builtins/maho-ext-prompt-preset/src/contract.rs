//! Port of `prompt-preset/contract.ts` (senpi 59d58cee59): the shared model-to-preset contract.
//!
//! The runtime resolver (`presets::resolve_preset_name`) handles the explicit setting and the
//! model's `promptPreset` metadata, then delegates here, so the runtime and the contract cannot
//! drift. Matcher order is load-bearing: a dotted or dated release resolves before its generic
//! family substring.

use crate::settings::PromptPresetName;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// `{ providerID, modelID, name? }`.
#[derive(Clone, Copy, Debug)]
pub struct ContractModel<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub name: Option<&'a str>,
}

fn normalize_model_id(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_whitespace = false;
    for character in lower.chars() {
        if character.is_whitespace() {
            if !in_whitespace {
                out.push('-');
            }
            in_whitespace = true;
        } else {
            out.push(character);
            in_whitespace = false;
        }
    }
    out
}

/// Matcher patterns are fixed literals; compile each once.
fn compiled(pattern: &'static str) -> Option<Arc<regex::Regex>> {
    type Cache = std::collections::HashMap<&'static str, Option<Arc<regex::Regex>>>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache
        .entry(pattern)
        .or_insert_with(|| regex::Regex::new(pattern).ok().map(Arc::new))
        .clone()
}

fn matches(value: &str, pattern: &'static str) -> bool {
    compiled(pattern).is_some_and(|regex| regex.is_match(&normalize_model_id(value)))
}

fn id_or_name(model: ContractModel<'_>, signal: impl Fn(&str) -> bool) -> bool {
    signal(model.model_id) || model.name.is_some_and(&signal)
}

fn any_marker(model_id: &str, markers: &[&str]) -> bool {
    let normalized = normalize_model_id(model_id);
    markers.iter().any(|marker| normalized.contains(marker))
}

const GPT6_FAMILY: &str =
    r"(?:^|[/@:._-])gpt[._-]?6(?:[._-]\d+|\d)?[._-](?:astra|sol|luna)(?:$|[/@:._-])";
const KIMI_K31: &str = r"(?:^|[/@._-])(?:kimi-)?k3(?:[._-]|p)1(?:$|[/@._:-])";
const KIMI_K3: &str = r"(?:^|[/@._-])kimi-k3(?:$|[/@._:-])";
const SWE2: &str = r"(?:^|[/@:._-])swe-2-(?:medium|high|max)(?:$|[/@:._])";
const KIMI_K28: &str = r"(?:^|[/@._-])kimi-k2(?:[._-]|p)8(?:$|[/@._:-])";
const KIMI_K27: &str = r"(?:^|[/@._-])kimi-k2(?:[._-]|p)7(?:$|[/@._:-])";
const KIMI_K26: &str = r"(?:^|[/@._-])kimi-k2(?:[._-]|p)6(?:$|[/@._:-])";
const GLM53: &str = r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)3(?:$|[/@._:-])";
const GLM52: &str = r"(?:^|[/@._-])glm(?:[._-]|p)5(?:[._-]|p)2(?:$|[/@._:-])";
const DEEPSEEK_V4_FLASH_0731: &str =
    r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash[._-]0731(?:$|[/@:._-])";
const DEEPSEEK_V41_FLASH: &str =
    r"(?:^|[/@:._-])deepseek[._-]v4(?:[._-]1|p1)[._-]flash(?:$|[/@:._-])";
const DEEPSEEK_FLASH: &str = r"(?:^|[/@:._-])deepseek[._-]flash(?:$|[/@:._-])";
const DEEPSEEK_V4_FLASH: &str = r"(?:^|[/@:._-])deepseek[._-]v4[._-]flash(?:$|[/@:._-])";
const DEEPSEEK_V4_PRO: &str = r"(?:^|[/@:._-])deepseek[._-]v4[._-]pro(?:$|[/@:._-])";
const GROK47: &str = r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?7(?:$|[/@._:-])";
const GROK46: &str = r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?6(?:$|[/@._:-])";
const GROK45: &str = r"(?:^|[/@:._-])grok(?:[._-]|p)?4(?:[._-]|p)?5(?:$|[/@._:-])";
const GEMINI4_ARGON: &str = r"(?:^|[/@:._-])gemini[._-]?4[._-]argon(?:$|[/@:._-])";

/// Kimi Code's rolling product ids (K2.8 Preview and K2.7 Code HighSpeed).
pub const KIMI_CODE_K27_MODEL_ID: &str = "kimi-for-coding-highspeed";
pub const KIMI_CODE_K28_MODEL_ID: &str = "kimi-for-coding";
pub const DEEPSEEK_OFFICIAL_PROVIDER: &str = "deepseek";

pub use maho_ai::utils::fable55_launch::is_fable55_model_id;

fn gpt5_version(model_id: &str) -> Option<PromptPresetName> {
    let normalized = normalize_model_id(model_id);
    [
        ("gpt-5.6", PromptPresetName::Gpt56),
        ("gpt-5.5", PromptPresetName::Gpt55),
        ("gpt-5.4", PromptPresetName::Gpt54),
        ("gpt-5.3", PromptPresetName::Gpt53Codex),
        ("gpt-5.2", PromptPresetName::Gpt52),
    ]
    .into_iter()
    .find_map(|(marker, preset)| normalized.contains(marker).then_some(preset))
}

fn claude_opus_version(model_id: &str) -> Option<PromptPresetName> {
    let normalized = normalize_model_id(model_id);
    if normalized.contains("opus-4-8") {
        Some(PromptPresetName::ClaudeOpus48)
    } else if normalized.contains("opus-4-7") {
        Some(PromptPresetName::ClaudeOpus47)
    } else if normalized.contains("opus-4-6") {
        Some(PromptPresetName::ClaudeOpus46)
    } else if normalized.contains("opus-4-5") || normalized.contains("opus-4.5") {
        Some(PromptPresetName::ClaudeOpus45)
    } else {
        None
    }
}

fn kimi_k3(value: &str) -> bool {
    normalize_model_id(value) == "k3" || matches(value, KIMI_K3)
}
fn kimi_k28(value: &str) -> bool {
    normalize_model_id(value) == KIMI_CODE_K28_MODEL_ID || matches(value, KIMI_K28)
}
fn kimi_k27(value: &str) -> bool {
    normalize_model_id(value) == KIMI_CODE_K27_MODEL_ID || matches(value, KIMI_K27)
}
fn deepseek_v41_flash(value: &str) -> bool {
    matches(value, DEEPSEEK_V41_FLASH) || matches(value, DEEPSEEK_FLASH)
}

/// senpi `resolvePresetName` (contract): the auto-resolved preset for a model, or `None`.
pub fn resolve_preset_name(model: ContractModel<'_>) -> Option<PromptPresetName> {
    use PromptPresetName as P;
    let id = model.model_id;
    let signal = |pattern: &'static str| id_or_name(model, |value| matches(value, pattern));
    if signal(GPT6_FAMILY) {
        return Some(P::Gpt6Astra);
    }
    if let Some(version) = gpt5_version(id) {
        return Some(version);
    }
    if signal(KIMI_K31) {
        return Some(P::KimiK31);
    }
    if signal(SWE2) || id_or_name(model, kimi_k3) {
        return Some(P::KimiK3);
    }
    if id_or_name(model, kimi_k28) {
        return Some(P::KimiK28);
    }
    if id_or_name(model, kimi_k27) {
        return Some(P::KimiK27);
    }
    if signal(KIMI_K26) {
        return Some(P::KimiK26);
    }
    // The dotted release must resolve before the generic fable-5 substring.
    if is_fable55_model_id(id) {
        return Some(P::ClaudeFable55);
    }
    if any_marker(id, &["fable-5-1", "fable-5.1", "mythos-5-1", "mythos-5.1"]) {
        return Some(P::ClaudeFable51);
    }
    if any_marker(id, &["fable-5", "mythos-5"]) {
        return Some(P::ClaudeFable5);
    }
    // The dotted release must resolve before the generic opus-5 substring.
    if any_marker(id, &["opus-5-5", "opus-5.5"]) {
        return Some(P::ClaudeOpus55);
    }
    if any_marker(id, &["opus-5"]) {
        return Some(P::ClaudeOpus5);
    }
    if any_marker(id, &["sonnet-5-5", "sonnet-5.5"]) {
        return Some(P::ClaudeSonnet55);
    }
    if any_marker(id, &["haiku-5-5", "haiku-5.5"]) {
        return Some(P::ClaudeHaiku55);
    }
    if let Some(version) = claude_opus_version(id) {
        return Some(version);
    }
    if signal(GLM53) {
        return Some(P::Glm53);
    }
    if signal(GLM52) {
        return Some(P::Glm52);
    }
    // The dated snapshot must resolve before the generic flash alias.
    if signal(DEEPSEEK_V4_FLASH_0731) {
        return Some(P::DeepseekV4Flash0731);
    }
    // DeepSeek retired V4 Flash on its official API: the alias is served by V4.1 Flash there.
    if id_or_name(model, deepseek_v41_flash)
        || (model.provider_id == DEEPSEEK_OFFICIAL_PROVIDER && matches(id, DEEPSEEK_V4_FLASH))
    {
        return Some(P::DeepseekV41Flash);
    }
    if signal(DEEPSEEK_V4_FLASH) {
        return Some(P::DeepseekV4Flash);
    }
    if signal(DEEPSEEK_V4_PRO) {
        return Some(P::DeepseekV4Pro);
    }
    if signal(GROK47) {
        return Some(P::Grok47);
    }
    if signal(GROK46) {
        return Some(P::Grok46);
    }
    if signal(GROK45) {
        return Some(P::Grok45);
    }
    if signal(GEMINI4_ARGON) {
        return Some(P::Gemini4Argon);
    }
    None
}

/// `PromptPresetModelCase`.
#[derive(Clone, Copy, Debug)]
pub struct PromptPresetModelCase {
    pub provider_id: &'static str,
    pub model_id: &'static str,
    pub preset: PromptPresetName,
}

const fn case(
    provider_id: &'static str,
    model_id: &'static str,
    preset: PromptPresetName,
) -> PromptPresetModelCase {
    PromptPresetModelCase {
        provider_id,
        model_id,
        preset,
    }
}

pub const PROMPT_PRESET_MODEL_CASES: &[PromptPresetModelCase] = {
    use PromptPresetName as P;
    &[
        case("openai", "gpt-6-astra", P::Gpt6Astra),
        case("openai", "gpt-5.6", P::Gpt56),
        case("openai", "gpt-5.5", P::Gpt55),
        case("openai", "gpt-5.4", P::Gpt54),
        case("openai", "gpt-5.3-codex", P::Gpt53Codex),
        case("openai", "gpt-5.2", P::Gpt52),
        case("kimi-coding", "k3.1-pro", P::KimiK31),
        case("moonshotai", "kimi-k3", P::KimiK3),
        case("kimi-for-coding", "kimi-for-coding", P::KimiK28),
        case("kimi-for-coding", "kimi-for-coding-highspeed", P::KimiK27),
        case("moonshotai", "kimi-k2.6", P::KimiK26),
        case("zai-coding-plan", "glm-5.3", P::Glm53),
        case("zai-coding-plan", "glm-5.2", P::Glm52),
        case("deepseek", "deepseek-v4-flash-0731", P::DeepseekV4Flash0731),
        case("deepseek", "deepseek-flash", P::DeepseekV41Flash),
        case(
            "fireworks",
            "accounts/fireworks/models/deepseek-v4-flash",
            P::DeepseekV4Flash,
        ),
        case("deepseek", "deepseek-v4-pro", P::DeepseekV4Pro),
        case("anthropic", "claude-fable-5", P::ClaudeFable5),
        case("anthropic", "claude-fable-5.1", P::ClaudeFable51),
        case("anthropic", "claude-fable-5-5", P::ClaudeFable55),
        case("anthropic", "claude-opus-5.5", P::ClaudeOpus55),
        case("anthropic", "claude-opus-5", P::ClaudeOpus5),
        case("anthropic", "claude-sonnet-5.5", P::ClaudeSonnet55),
        case("anthropic", "claude-haiku-5.5", P::ClaudeHaiku55),
        case("anthropic", "claude-opus-4-8", P::ClaudeOpus48),
        case("anthropic", "claude-opus-4-7", P::ClaudeOpus47),
        case("anthropic", "claude-opus-4-6", P::ClaudeOpus46),
        case("anthropic", "claude-opus-4-5", P::ClaudeOpus45),
        case("xai", "grok-4.7", P::Grok47),
        case("xai", "grok-4.6", P::Grok46),
        case("xai", "grok-4.5", P::Grok45),
        case("google", "gemini-4-argon", P::Gemini4Argon),
    ]
};

/// Presets a model id can auto-resolve to. gpt-5 is manual-only (no model matcher), so it is absent.
pub const AUTO_RESOLVED_PRESET_NAMES: &[PromptPresetName] = {
    use PromptPresetName as P;
    &[
        P::Gpt6Astra,
        P::Gpt56,
        P::Gpt55,
        P::Gpt54,
        P::Gpt53Codex,
        P::Gpt52,
        P::KimiK31,
        P::KimiK3,
        P::KimiK28,
        P::KimiK27,
        P::KimiK26,
        P::Glm53,
        P::Glm52,
        P::DeepseekV4Flash0731,
        P::DeepseekV41Flash,
        P::DeepseekV4Flash,
        P::DeepseekV4Pro,
        P::Grok47,
        P::Grok46,
        P::Grok45,
        P::ClaudeFable5,
        P::ClaudeFable51,
        P::ClaudeFable55,
        P::ClaudeOpus55,
        P::ClaudeOpus5,
        P::ClaudeSonnet55,
        P::ClaudeHaiku55,
        P::ClaudeOpus48,
        P::ClaudeOpus47,
        P::ClaudeOpus46,
        P::ClaudeOpus45,
        P::Gemini4Argon,
    ]
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::ModelMetadata;

    fn contract(provider_id: &str, model_id: &str) -> Option<PromptPresetName> {
        resolve_preset_name(ContractModel {
            provider_id,
            model_id,
            name: None,
        })
    }

    fn runtime(provider: &str, id: &str) -> Option<PromptPresetName> {
        crate::presets::resolve_preset_name(
            ModelMetadata {
                id,
                provider,
                name: None,
                prompt_preset: None,
            },
            PromptPresetName::Auto,
        )
    }

    #[test]
    fn resolves_every_shared_model_case_to_its_preset() {
        for case in PROMPT_PRESET_MODEL_CASES {
            assert_eq!(
                contract(case.provider_id, case.model_id),
                Some(case.preset),
                "{}/{}",
                case.provider_id,
                case.model_id
            );
            assert_eq!(
                runtime(case.provider_id, case.model_id),
                Some(case.preset),
                "runtime {}/{}",
                case.provider_id,
                case.model_id
            );
        }
    }

    #[test]
    fn covers_every_auto_resolved_preset_with_a_case() {
        for name in AUTO_RESOLVED_PRESET_NAMES {
            assert!(
                PROMPT_PRESET_MODEL_CASES
                    .iter()
                    .any(|case| case.preset == *name),
                "no case resolves to {}",
                name.as_str()
            );
        }
    }

    #[test]
    fn exposes_exactly_the_non_auto_valid_presets() {
        let mut auto: Vec<&str> = crate::settings::VALID_PRESETS
            .iter()
            .copied()
            .filter(|preset| *preset != "auto" && *preset != "gpt-5")
            .collect();
        auto.sort_unstable();
        let mut exposed: Vec<&str> = AUTO_RESOLVED_PRESET_NAMES
            .iter()
            .map(|name| name.as_str())
            .collect();
        exposed.sort_unstable();
        assert_eq!(exposed, auto);
    }

    #[test]
    fn unknown_model_resolves_to_none() {
        assert_eq!(contract("acme", "acme-unknown-9000"), None);
    }

    #[test]
    fn probe_ids_resolve_identically_through_runtime_and_contract() {
        let probes = [
            ("anthropic", "opus-6-preview", None),
            ("openai", "gpt-5.7-preview", None),
            (
                "anthropic",
                "opus-5.5",
                Some(PromptPresetName::ClaudeOpus55),
            ),
            (
                "deepseek",
                "deepseek-v4-flash",
                Some(PromptPresetName::DeepseekV41Flash),
            ),
            (
                "fireworks",
                "accounts/fireworks/models/deepseek-v4-flash",
                Some(PromptPresetName::DeepseekV4Flash),
            ),
        ];
        for (provider, id, expected) in probes {
            assert_eq!(runtime(provider, id), expected, "{provider}/{id} runtime");
            assert_eq!(contract(provider, id), expected, "{provider}/{id} contract");
        }
    }

    #[test]
    fn fable55_ids_resolve_to_launch_baseline() {
        for id in [
            "claude-fable-5-5",
            "claude-fable-5.5",
            "anthropic/claude-mythos-5-5",
        ] {
            assert_eq!(
                runtime("anthropic", id),
                Some(PromptPresetName::ClaudeFable55)
            );
        }
        assert!(!is_fable55_model_id("claude-fable-5-50"));
        assert!(!is_fable55_model_id("claude-fable-5"));
    }

    #[test]
    fn devin_serves_only_the_medium_high_max_swe2_lanes() {
        for id in [
            "swe-2-medium",
            "swe-2-high",
            "swe-2-max",
            "devin/swe-2-medium",
            "devin/swe-2-high",
        ] {
            assert_eq!(
                contract("devin", id),
                Some(PromptPresetName::KimiK3),
                "{id}"
            );
        }
        for id in ["swe-2", "swe-2-low", "swe-2-high-lite"] {
            assert_ne!(
                contract("devin", id),
                Some(PromptPresetName::KimiK3),
                "{id}"
            );
        }
    }

    #[test]
    fn gpt61_point_release_and_dotless_spelling_route_to_astra() {
        for id in [
            "gpt-6.1-sol",
            "gpt-6.1-sol-fast",
            "openai/gpt-6.1-sol",
            "openai-gpt-61-sol",
        ] {
            assert_eq!(
                contract("openai", id),
                Some(PromptPresetName::Gpt6Astra),
                "{id}"
            );
        }
        for id in ["gpt-6.1", "gpt-61", "gpt-6"] {
            assert_eq!(contract("openai", id), None, "{id}");
        }
    }

    #[test]
    fn parts_reassemble_exact_provider_prompt_and_keep_project_source() {
        use maho_core::dynamic_prompt::{PromptSurface, build::BuildDynamicSystemPromptOptions};
        let preset = crate::presets::resolve_preset(
            ModelMetadata {
                id: "claude-fable-5-5",
                provider: "anthropic",
                name: None,
                prompt_preset: None,
            },
            PromptPresetName::Auto,
            BuildDynamicSystemPromptOptions {
                append_system_prompt: None,
                custom_prompt: None,
                on_parts: None,
                cwd: "/work".into(),
                selected_tools: vec![],
                tool_snippets: Default::default(),
                prompt_guidelines: vec![],
                context_files: vec![maho_core::system_prompt::ContextFile {
                    path: "/work/AGENTS.md".into(),
                    content: "rules".into(),
                    scope: None,
                }],
                skills: vec![],
                tuning_section: None,
                core_prompt: None,
                workstation_dialect: None,
                surface: PromptSurface::Terminal,
            },
        )
        .expect("preset");
        assert_eq!(
            preset
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<String>(),
            preset.prompt
        );
        assert!(
            preset
                .parts
                .iter()
                .any(|part| part.source.as_deref() == Some("/work/AGENTS.md"))
        );
        assert_eq!(
            preset.parts[0].source.as_deref(),
            Some("prompt-preset:claude-fable-5-5")
        );
    }
}
