//! Builtin delegation categories and their model resolution (`category/` in TypeScript).

mod builtins;
mod fallback_chains;
mod prompts;
mod resolver;

pub use builtins::{
    BUILTIN_CATEGORY_DEFAULTS, BuiltinCategoryConfig, BuiltinCategoryDefinition, builtin_category,
    builtin_category_requires_model, category_gate_model, is_category_chain_rung_resolvable,
    is_category_chain_viable, is_category_gate_satisfied, resolve_deep_category_prompt_append,
};
pub use fallback_chains::{CATEGORY_FALLBACK_CHAINS, category_fallback_chain};
pub use resolver::{
    CategoryModelSelection, CategoryResolutionResult, ModelUnavailable, ResolveCategoryOptions,
    ResolvedChildSpec, resolve_available_category_names, resolve_category,
};

#[cfg(test)]
#[path = "category_tests.rs"]
mod tests;
