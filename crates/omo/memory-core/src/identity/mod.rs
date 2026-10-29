//! Memory identity resolution: layout paths plus the deterministic id builder.

pub mod git_layout;
pub mod layout;
pub mod project_scope;
pub mod project_scope_fs;
pub use project_scope::{
    ProjectIdentity, ProjectIdentityResolver, ProjectIdentityResolverOptions,
    create_project_identity_resolver,
};
pub use resolve::{AutoIdentityMode, resolve_auto_identity, resolve_auto_identity_with_probe};
pub mod resolve;

pub use layout::{
    AGENTS_DIRNAME, MEMORY_ROOT_ENV_VAR, MemoryIdentityPaths, REPO_DIRNAME, RUNTIME_DIRNAME,
    RUNTIME_SUBDIRNAMES, build_identity_paths, default_memory_root, home_directory,
    resolve_memory_root,
};
pub use resolve::{
    AUTO_AGENT_VALUE, FALLBACK_SLUG, IdentityError, MAX_SLUG_LENGTH, MemoryIdentity,
    SHORT_HASH_LENGTH, is_auto_agent_value, resolve_memory_identity, sanitize_to_slug, short_hash,
};
