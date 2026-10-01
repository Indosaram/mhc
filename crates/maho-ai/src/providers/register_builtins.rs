//! Port of senpi packages/ai/src/providers/register-builtins.ts, whose whole contract is the one
//! line import "../compat.ts". Rust has no import side effects, so the builtin API/provider
//! registration that import triggers lives in crate::compat (todo 10); importing this module in TS
//! is expressed here as re-exporting it, so callers keep a single reachable name for the contract.

pub use crate::compat;
