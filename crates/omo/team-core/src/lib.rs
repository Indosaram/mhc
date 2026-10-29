//! Rust port of the `@oh-my-opencode/team-core` package (SUL-1.0, internal use only).
//!
//! Multi-agent team orchestration: team specs and registry, runtime state machine with
//! file locks, mailbox/steering delivery, shared tasklist, per-member worktrees and the
//! tmux team layout.

mod clock;
pub mod config;
pub mod error;
pub mod logger;
pub mod member_parser;
mod path_util;
pub mod resolve_caller_team_lead;
pub mod session_client;
pub mod shell_quote;
pub mod team_layout_tmux;
pub mod team_mailbox;
pub mod team_registry;
pub mod team_state_store;
pub mod team_tasklist;
pub mod team_worktree;
mod tolerant_fsync;
pub mod types;

pub use config::TeamModeConfig;
pub use error::{Result, TeamCoreError};
