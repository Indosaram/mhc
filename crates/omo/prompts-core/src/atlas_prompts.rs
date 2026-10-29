use crate::types::{BundledPromptSource, PromptSource, VariantTable};

#[must_use]
pub fn atlas_prompt_variants() -> VariantTable {
    VariantTable::from([
        (
            "opus-4-7",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/opus-4-7.md"),
                file_path: "packages/prompts-core/prompts/atlas/opus-4-7.md",
            }),
        ),
        (
            "gpt",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/gpt.md"),
                file_path: "packages/prompts-core/prompts/atlas/gpt.md",
            }),
        ),
        (
            "gemini",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/gemini.md"),
                file_path: "packages/prompts-core/prompts/atlas/gemini.md",
            }),
        ),
        (
            "kimi-k3-1",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/kimi-k3-1.md"),
                file_path: "packages/prompts-core/prompts/atlas/kimi-k3-1.md",
            }),
        ),
        (
            "kimi-k3",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/kimi-k3.md"),
                file_path: "packages/prompts-core/prompts/atlas/kimi-k3.md",
            }),
        ),
        (
            "swe-2",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/kimi-k3.md"),
                file_path: "packages/prompts-core/prompts/atlas/kimi-k3.md",
            }),
        ),
        (
            "kimi-k2-7",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/kimi-k2-7.md"),
                file_path: "packages/prompts-core/prompts/atlas/kimi-k2-7.md",
            }),
        ),
        (
            "kimi",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/kimi.md"),
                file_path: "packages/prompts-core/prompts/atlas/kimi.md",
            }),
        ),
        (
            "glm",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/glm.md"),
                file_path: "packages/prompts-core/prompts/atlas/glm.md",
            }),
        ),
        (
            "default",
            PromptSource::Bundled(BundledPromptSource {
                content: include_str!("../prompts/atlas/default.md"),
                file_path: "packages/prompts-core/prompts/atlas/default.md",
            }),
        ),
    ])
}
