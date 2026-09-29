//! In-memory and filesystem abstractions for memory-core.

pub mod frontmatter;
pub mod hooks;
pub mod hooks_scripts;
pub mod paths;

pub use frontmatter::{
    FrontmatterError, MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
    sanitize_frontmatter_value,
};
pub use hooks::{InstalledHook, install_hooks, resolve_hooks_dir};
pub use hooks_scripts::{POST_COMMIT_HOOK_SCRIPT, PRE_COMMIT_HOOK_SCRIPT};
pub use paths::{
    MemoryPathError, ValidateMemoryPathOptions, validate_memory_path, validate_repository_path,
    validate_repository_path_with_field,
};
