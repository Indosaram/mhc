//! Harness-neutral primitives for the `omo.json` config surface.
//!
//! Rust port of the TypeScript package `@oh-my-opencode/omo-config-core`. The
//! public surface keeps the TypeScript entry points: a schema tree with
//! defaults and strict unknown-key rejection, a walked multi-layer loader with
//! VSCode-style view resolution, a shared model catalog resolver, a
//! comment-preserving atomic writer, and a lock+journal legacy-config migration
//! engine. All IO goes through injectable filesystem ports.
//!
//! See `parity.md` in the crate root for the test-by-test mapping back to the
//! TypeScript sources.

pub mod internal;
pub mod issue;
pub mod loader;
pub mod migration;
pub mod models;
pub mod schema;
pub mod writer;

pub use internal::{is_plain_object, is_unsafe_object_key, parse_jsonc_safe, to_posix_path};
pub use issue::{Issue, IssueCode, Issues, PathSegment};
// `loader` and `writer` both contain a `types` submodule; the crate-root
// `types` name is intentionally left ambiguous; callers use
// `loader::types` / `writer::types`, while the items inside stay re-exported.
#[allow(ambiguous_glob_reexports)]
pub use loader::*;
pub use migration::*;
pub use models::*;
pub use schema::*;
pub use writer::*;
