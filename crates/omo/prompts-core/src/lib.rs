mod types;
mod atlas_prompts;
mod prometheus_prompts;
mod ultrawork_prompts;
mod variant_resolver;
mod loader;
mod mode_prompts;

pub use types::*;
pub use atlas_prompts::atlas_prompt_variants;
pub use prometheus_prompts::prometheus_prompt_variants;
pub use ultrawork_prompts::*;
pub use variant_resolver::{resolve_variant, ResolveVariantInput, EmptyVariantTableError};
pub use loader::{load_prompt, load_prompt_sync, PromptFileNotFoundError, PromptPathTraversalError, PromptLoadError};
pub use mode_prompts::{HYPERPLAN_MODE_PROMPT, TEAM_MODE_PROMPT};

#[must_use]
pub fn resolve_atlas_variant(model_id: &str) -> &'static str {
    // Atlas has a default entry, so resolution is infallible for this table.
    resolve_variant(ResolveVariantInput {
        model_id: Some(model_id), agent_name: None, variants: &atlas_prompt_variants(),
    }).unwrap_or("default")
}
