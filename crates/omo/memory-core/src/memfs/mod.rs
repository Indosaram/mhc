//! In-memory and filesystem abstractions for memory-core.

pub mod frontmatter;
pub mod frontmatter_entries;
pub mod frontmatter_scalar;
pub mod frontmatter_validation;
pub mod hooks;
pub mod hooks_commit_msg;
pub mod hooks_scripts;
pub mod lookalike_text;
pub mod memory_hook;
pub mod memory_hook_cli;
pub mod memory_hook_git;
pub mod normalize;
pub mod paths;

pub use frontmatter::{
    FrontmatterError, MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
    sanitize_frontmatter_value,
};
pub use frontmatter_entries::{
    FrontmatterEntry, content_frontmatter_entries, duplicate_frontmatter_keys,
    frontmatter_entries, look_alike_frontmatter_keys, normalized_frontmatter_key,
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
pub use hooks::{
    InstallHooksOptions, InstalledHook, install_hooks, install_hooks_with_options,
    resolve_common_git_dir, resolve_hooks_dir,
};
pub use hooks_commit_msg::{
    COMMIT_MSG_HOOK_SCRIPT, MemoryHookChecker, with_memory_hook_checker,
};
pub use hooks_scripts::{
    HOOK_SECRET_PATTERNS, HookSecretPattern, POST_COMMIT_HOOK_SCRIPT, PRE_COMMIT_HOOK_SCRIPT,
};
pub use lookalike_text::look_alike_skeleton;
pub use memory_hook::{
    commit_message_failures, pre_commit_failures, provenance_of, run_memory_hook,
};
pub use memory_hook_cli::run_cli;
pub use memory_hook_git::{
    StagedEntry, merge_heads, parse_raw_z, read_blob, staged_entries, tree_blob,
};
pub use normalize::{
    FRONTMATTER_NORMALIZATION_MARKER, FRONTMATTER_NORMALIZATION_SUBJECT,
    NormalizeFrontmatterResult, NormalizeFrontmatterSkip, frontmatter_normalization_pending,
    normalize_memory_frontmatter,
};
pub use paths::{
    MemoryPathError, ValidateMemoryPathOptions, is_memory_content_path, validate_memory_path,
    validate_repository_path, validate_repository_path_with_field,
};
