//! In-memory and filesystem abstractions for memory-core.

pub mod frontmatter;
pub mod frontmatter_scalar;
pub mod frontmatter_validation;
pub mod hooks;
pub mod hooks_scripts;
pub mod paths;

pub use frontmatter::{
    FrontmatterError, MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
    sanitize_frontmatter_value,
};
pub use frontmatter_scalar::{
    decode_quoted_scalar, decode_scalar_source, decode_string_array, is_canonical_scalar_source,
    is_plain_safe_scalar, is_quoted_scalar_source, render_raw_scalar, render_string_scalar,
    split_frontmatter,
};
pub use frontmatter_validation::{
    MAX_DESCRIPTION_LENGTH, describe_description_violation, describe_frontmatter_grammar_violation,
    describe_frontmatter_violation, describe_header_grammar_violation, describe_header_violation,
    find_tool_call_scaffolding,
};
pub use hooks::{InstalledHook, install_hooks, resolve_hooks_dir};
pub use hooks_scripts::{POST_COMMIT_HOOK_SCRIPT, PRE_COMMIT_HOOK_SCRIPT};
pub use paths::{
    MemoryPathError, ValidateMemoryPathOptions, is_memory_content_path, validate_memory_path, validate_repository_path,
    validate_repository_path_with_field,
};
