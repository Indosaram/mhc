//! Prompt compilation, memory rendering, and revision caching.

pub mod cache;
pub mod changes;
#[allow(clippy::module_inception)] // mirrors the TS compile/compile.ts layout
pub mod compile;
pub mod render;

#[cfg(test)]
#[path = "compile_test_support.rs"]
pub mod compile_test_support;

pub use cache::{MEMORY_TEMPLATE_STRUCTURE_VERSION, MemoryBlockCache, hash_memory_template};
pub use changes::{
    MEMORY_SESSION_TRAILER, ProjectedChanges, ProjectedChangesOptions,
    is_empty_projected_changes, projected_changes_between, revision_exists,
};
pub use compile::{
    CompileError, CompileMemoryBlockOptions, compile_memory_block, compile_memory_block_at_revision,
};
pub use render::{
    CompiledSystemFile, RenderError, mark_memory_block, render_external_projection,
    render_system_tree, replace_memory_block, strip_memory_block,
};
