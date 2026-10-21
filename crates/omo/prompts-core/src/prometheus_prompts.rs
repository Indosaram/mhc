use crate::types::{BundledPromptSource, PromptSource, VariantTable};

#[must_use]
pub fn prometheus_prompt_variants() -> VariantTable {
    VariantTable::from([
        ("default", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/prometheus/default.md"),
            file_path: "packages/prompts-core/prompts/prometheus/default.md",
        })),
    ])
}
