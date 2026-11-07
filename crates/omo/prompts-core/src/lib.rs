mod atlas_prompts;
mod loader;
mod mode_prompts;
mod prometheus_prompts;
mod types;
mod variant_resolver;

pub use atlas_prompts::atlas_prompt_variants;
pub use loader::{
    PromptFileNotFoundError, PromptLoadError, PromptPathTraversalError, load_prompt,
    load_prompt_sync,
};
pub use mode_prompts::{HYPERPLAN_MODE_PROMPT, TEAM_MODE_PROMPT};
pub use prometheus_prompts::prometheus_prompt_variants;
pub use types::*;
pub use variant_resolver::{EmptyVariantTableError, ResolveVariantInput, resolve_variant};

#[must_use]
pub fn resolve_atlas_variant(model_id: &str) -> &'static str {
    // Atlas has a default entry, so resolution is infallible for this table.
    resolve_variant(ResolveVariantInput {
        model_id: Some(model_id),
        agent_name: None,
        variants: &atlas_prompt_variants(),
    })
    .unwrap_or("default")
}
