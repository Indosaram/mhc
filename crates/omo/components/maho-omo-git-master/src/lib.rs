//! Port of omo-senpi `components/git-master` at omo `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`:
//! the `git-master-attribution` component. One native `tool_result` hook appends the
//! commit-footer directive after a successful read of the `/git-master/SKILL.md` skill, using the
//! `git_master` settings resolved from the effective omo config for the session cwd.
//!
//! Upstream `index.ts` maps to [`index`] and `directive.ts` to [`directive`], keeping names and
//! order so an upstream diff translates mechanically. The TS barrel (`components/git-master`
//! resolves through `index.ts`) has no separate file; `index.ts` re-exports nothing, so the native
//! `lib.rs` mirrors the todo-fanout crate's shape and re-exports [`index`].
pub mod directive;
pub mod index;
pub use index::*;
