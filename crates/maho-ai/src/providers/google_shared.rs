//! Port of senpi packages/ai/src/providers/google-shared.ts (`export * from "../api/google-shared.ts"`).
//!
//! `api/google-shared.ts` is ported by todo 11; until it lands the glob resolves to no items and
//! rustc reports `unused_imports`. The allow is scoped to that empty-seam state and is not needed
//! once the wire module carries its items.

#[allow(unused_imports)]
pub use crate::api::google_shared::*;
