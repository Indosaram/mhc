use std::collections::BTreeMap;

use model_core::{
    is_claude_opus47_model, is_gemini_model, is_glm_model, is_gpt_model, is_kimi_k2_code_model,
    is_kimi_k2_model, is_kimi_k3_model, is_mini_max_model, is_swe2_model,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptVariant {
    pub content: &'static str,
    pub file_path: &'static str,
}

#[must_use]
pub fn atlas_prompt_variants(kimi_k3_prompt: &'static str) -> BTreeMap<&'static str, PromptVariant> {
    let kimi_k3 = PromptVariant {
        content: kimi_k3_prompt,
        file_path: "packages/prompts-core/prompts/atlas/kimi-k3.md",
    };
    BTreeMap::from([
        ("swe-2", kimi_k3.clone()),
        ("kimi-k3", kimi_k3),
    ])
}

#[must_use]
pub fn resolve_atlas_variant(model_id: &str) -> &'static str {
    if is_swe2_model(model_id) {
        "swe-2"
    } else if is_kimi_k3_model(model_id) {
        "kimi-k3"
    } else if is_kimi_k2_code_model(model_id) {
        "kimi-k2-7"
    } else if is_kimi_k2_model(model_id) {
        "kimi"
    } else if is_gpt_model(model_id) {
        "gpt"
    } else if is_gemini_model(model_id) {
        "gemini"
    } else if is_glm_model(model_id) {
        "glm"
    } else if is_claude_opus47_model(model_id) {
        "opus-4-7"
    } else if is_mini_max_model(model_id) {
        "minimax"
    } else {
        "default"
    }
}

#[cfg(test)]
mod tests {
    use super::{atlas_prompt_variants, resolve_atlas_variant};

    #[test]
    fn atlas_routes_swe2_to_shared_kimi_k3_prompt() {
        let variants = atlas_prompt_variants("kimi k3 prompt");
        assert_eq!(resolve_atlas_variant("devin/swe-2-high"), "swe-2");
        assert_eq!(variants["swe-2"], variants["kimi-k3"]);
        assert_eq!(variants["swe-2"].content, "kimi k3 prompt");
    }

    #[test]
    fn atlas_does_not_route_other_swe_families_to_swe2() {
        assert_eq!(resolve_atlas_variant("devin/swe-1-7"), "default");
    }
}
