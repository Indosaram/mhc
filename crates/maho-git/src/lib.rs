//! Git support for mhc (MIT; see `PROVENANCE.md`): a small `git` command runner with the
//! repository queries task isolation needs ([`repo`]), unified-diff parsing and rendering
//! ([`diff`]), and the read-only git tools mounted as `<builtin:git>` in the MIT profile
//! ([`tools`]).
pub mod diff;
pub mod repo;
pub mod tools;

pub use repo::{ApplyOptions, Git, GitError, GitResult, NumstatEntry};
pub use tools::{GitToolsExtension, TOOL_NAMES};
