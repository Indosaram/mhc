//! Test support shared by every maho crate.
//!
//! - [`golden`]: byte-exact comparison against fixtures generated from senpi by `tools/golden`.
//!   There is no update mode: fixtures only come from the generator.
//! - [`vterm`]: a port of senpi's `packages/tui/test/virtual-terminal.ts` backed by the `vt100`
//!   crate, serializing screens in the same JSON cell format as `tools/golden/run.mjs`.
//! - [`faux`]: scripted-turn helpers reading the same `tools/golden/scripts/<name>.json` files
//!   that `tools/golden/faux-harness.mjs` feeds to senpi's faux provider.

pub mod faux;
pub mod golden;
pub mod vterm;

#[cfg(feature = "sdk")]
pub mod faux_session;
