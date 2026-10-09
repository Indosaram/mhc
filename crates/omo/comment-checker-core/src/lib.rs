//! Rust port of the `@oh-my-opencode/comment-checker-core` package (SUL-1.0, internal use only).

mod apply_patch_edits;
mod cached_release;
mod release;
mod runner;
mod types;

pub use apply_patch_edits::extract_apply_patch_edits;
pub use apply_patch_edits::get_apply_patch_metadata_files;
pub use apply_patch_edits::get_string;
pub use apply_patch_edits::is_record;
pub use apply_patch_edits::join_patch_lines;
pub use apply_patch_edits::make_accumulator;
pub use apply_patch_edits::parse_apply_patch_requests;
pub use apply_patch_edits::read_apply_patch_metadata_files;
pub use cached_release::COMMENT_CHECKER_VERSION_MARKER;
pub use cached_release::MarkerReader;
pub use cached_release::is_cached_comment_checker_current;
pub use cached_release::record_cached_comment_checker_release;
pub use release::COMMENT_CHECKER_RELEASE_REPO;
pub use release::COMMENT_CHECKER_RELEASE_VERSION;
pub use release::CommentCheckerArchiveExtension;
pub use release::CommentCheckerCacheDirInput;
pub use release::CommentCheckerReleaseAsset;
pub use release::comment_checker_binary_name;
pub use release::comment_checker_cache_dir;
pub use release::resolve_comment_checker_release_asset;
pub use runner::DEFAULT_KILL_GRACE_MS;
pub use runner::DEFAULT_PACKAGE_NAME;
pub use runner::DEFAULT_TIMEOUT_MS;
pub use runner::resolve_comment_checker_binary;
pub use runner::run_comment_checker;
pub use types::*;
