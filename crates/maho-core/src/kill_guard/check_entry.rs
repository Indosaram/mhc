//! Port of `packages/coding-agent/src/core/kill-guard/check-entry.ts` (senpi#5349, L2).
//!
//! In the compiled native binary, the hidden `__kill-guard` subcommand calls directly into
//! [`crate::kill_guard::check_command::run_kill_guard_check_command`]; this module mirrors the
//! upstream entry module.

pub use super::check_command::run_kill_guard_check_command as run;
