//! Prompt compilation, memory rendering, and revision caching.

pub mod block_persist;
pub mod cache;
pub mod changes;
#[allow(clippy::module_inception)] // mirrors the TS compile/compile.ts layout
pub mod compile;
pub mod expiry_slot;
pub mod external_projection;
pub mod external_projection_dirs;
pub mod external_projection_names;
pub mod render;
pub mod stored_block;

#[cfg(test)]
#[path = "compile_test_support.rs"]
pub mod compile_test_support;

pub use cache::{MEMORY_TEMPLATE_STRUCTURE_VERSION, MemoryBlockCache, hash_memory_template};
pub use changes::{
    MEMORY_SESSION_TRAILER, ProjectedChanges, ProjectedChangesOptions, is_empty_projected_changes,
    projected_changes_between, revision_exists,
};
pub use compile::{
    CompileError, CompileMemoryBlockOptions, CompiledMemoryBlock, MemoryExpiry,
    compile_memory_block, compile_memory_block_at_revision, compile_memory_block_with_reads,
    now_ms,
};
pub use expiry_slot::expiry_slot;
pub use render::{
    CompiledSystemFile, RenderError, mark_memory_block, render_external_projection,
    render_system_tree, replace_memory_block, strip_memory_block,
};
pub use stored_block::{StoredBlock, decode_stored_block, encode_stored_block};
