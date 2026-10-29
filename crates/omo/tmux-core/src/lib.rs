//! Rust port of the `@oh-my-opencode/tmux-core` package (SUL-1.0, internal use only).
//!
//! Harness-neutral tmux primitives: the command runner, pane/window/session
//! lifecycle, layout helpers and command-string building. Every lifecycle
//! function takes injected dependencies so adapters supply the tmux path,
//! logging and server-health checks.

mod cmux_detect;
mod constants;
mod env_source;
mod runner;
pub mod tmux_utils;
mod types;

pub use cmux_detect::{is_cmux_compat, is_cmux_compat_environment};
pub use constants::*;
pub use env_source::{EnvSource, ProcessEnv};
pub use runner::{RunTmuxOptions, TmuxCommandResult, run_tmux_command};
pub use tmux_utils::*;
pub use types::*;
