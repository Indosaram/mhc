use crate::types::VariantTable;
use model_core::{
    is_claude_opus47_model, is_gemini_model, is_glm_model, is_gpt_model, is_kimi_k2_code_model,
    is_kimi_k2_model, is_kimi_k3_model, is_kimi_k31_model, is_mini_max_model, is_swe2_model,
};

pub struct ResolveVariantInput<'a> {
    pub model_id: Option<&'a str>,
    pub agent_name: Option<&'a str>,
    pub variants: &'a VariantTable,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct EmptyVariantTableError;

impl std::fmt::Display for EmptyVariantTableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("resolveVariant requires at least one prompt variant")
    }
}
impl std::error::Error for EmptyVariantTableError {}

pub fn resolve_variant(
    input: ResolveVariantInput<'_>,
) -> Result<&'static str, EmptyVariantTableError> {
    let first = input.variants.first().ok_or(EmptyVariantTableError)?.0;
    if input
        .agent_name
        .is_some_and(|name| name.eq_ignore_ascii_case("prometheus"))
        && input.variants.contains_key("planner")
    {
        return Ok("planner");
    }
    if let Some(model) = input.model_id {
        for &name in input.variants.keys() {
            let matches = match name {
                "gpt" => is_gpt_model(model),
                "gemini" => is_gemini_model(model),
                "kimi-k3-1" => is_kimi_k31_model(model),
                "kimi-k3" => is_kimi_k3_model(model),
                "swe-2" => is_swe2_model(model),
                "kimi-k2-7" => is_kimi_k2_code_model(model),
                "kimi" => is_kimi_k2_model(model),
                "glm" => is_glm_model(model),
                "opus-4-7" => is_claude_opus47_model(model),
                "minimax" => is_mini_max_model(model),
                _ => false,
            };
            if matches {
                return Ok(name);
            }
        }
    }
    Ok(if input.variants.contains_key("default") {
        "default"
    } else {
        first
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilesystemPromptSource, PromptSource, atlas_prompt_variants};

    fn table(names: &[&'static str]) -> VariantTable {
        names
            .iter()
            .map(|&name| {
                (
                    name,
                    PromptSource::Filesystem(FilesystemPromptSource {
                        base_dir: format!("/prompts/{name}").into(),
                    }),
                )
            })
            .collect()
    }

    #[test]
    fn model_routing_and_planner_precedence() {
        let variants = table(&["planner", "gpt", "gemini", "kimi", "glm", "default"]);
        for (model, expected) in [
            ("claude-opus-4-7", "default"),
            ("gpt-5-5", "gpt"),
            ("gemini-3-1-pro", "gemini"),
            ("kimi-k2-6", "kimi"),
            ("glm-5-1", "glm"),
            ("claude-haiku-4-5", "default"),
        ] {
            assert_eq!(
                resolve_variant(ResolveVariantInput {
                    model_id: Some(model),
                    agent_name: None,
                    variants: &variants
                }),
                Ok(expected)
            );
        }
        assert_eq!(
            resolve_variant(ResolveVariantInput {
                model_id: Some("gpt-5-5"),
                agent_name: Some("PROMETHEUS"),
                variants: &variants
            }),
            Ok("planner")
        );
    }

    #[test]
    fn ordered_kimi_and_swe_routes() {
        let variants = table(&["swe-2", "kimi-k3", "kimi-k2-7", "kimi", "default"]);
        for (model, expected) in [
            ("kimi-k2.7", "kimi-k2-7"),
            ("kimi-k2.8", "kimi-k2-7"),
            ("kimi-for-coding/k2p8", "kimi-k2-7"),
            ("kimi-for-coding/kimi-for-coding", "kimi-k2-7"),
            ("kimi-for-coding/kimi-for-coding-highspeed", "kimi-k2-7"),
            ("kimi-k2-6", "kimi"),
            ("kimi-k3", "kimi-k3"),
            ("kimi-for-coding/k3p1", "kimi-k3"),
            ("devin/swe-2-high", "swe-2"),
            ("devin/swe-1-7", "default"),
        ] {
            assert_eq!(
                resolve_variant(ResolveVariantInput {
                    model_id: Some(model),
                    agent_name: None,
                    variants: &variants
                }),
                Ok(expected)
            );
        }
        let atlas = atlas_prompt_variants();
        assert_eq!(atlas["swe-2"], atlas["kimi-k3"]);
    }

    #[test]
    fn dedicated_k31_precedes_k3() {
        let variants = atlas_prompt_variants();
        for (model, expected) in [
            ("kimi-for-coding/k3p1", "kimi-k3-1"),
            ("kimi-k3", "kimi-k3"),
            ("swe-2", "swe-2"),
        ] {
            assert_eq!(
                resolve_variant(ResolveVariantInput {
                    model_id: Some(model),
                    agent_name: None,
                    variants: &variants
                }),
                Ok(expected)
            );
        }
        assert_ne!(variants["kimi-k3-1"], variants["kimi-k3"]);
    }

    #[test]
    fn empty_table_errors_and_first_variant_falls_back() {
        assert_eq!(
            resolve_variant(ResolveVariantInput {
                model_id: None,
                agent_name: None,
                variants: &VariantTable::new()
            }),
            Err(EmptyVariantTableError)
        );
        assert_eq!(
            resolve_variant(ResolveVariantInput {
                model_id: None,
                agent_name: None,
                variants: &table(&["custom", "gpt"])
            }),
            Ok("custom")
        );
    }
}
