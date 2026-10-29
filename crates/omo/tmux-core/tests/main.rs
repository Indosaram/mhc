//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "cmux_detect.rs"]
pub mod cmux_detect;
#[path = "layout.rs"]
pub mod layout;
#[path = "missing_tmux.rs"]
pub mod missing_tmux;
#[path = "pane_auth_cmux.rs"]
pub mod pane_auth_cmux;
#[path = "pane_auth_env.rs"]
pub mod pane_auth_env;
#[path = "pane_close.rs"]
pub mod pane_close;
#[path = "pane_command.rs"]
pub mod pane_command;
#[path = "pane_dimensions.rs"]
pub mod pane_dimensions;
#[path = "pane_replace.rs"]
pub mod pane_replace;
#[path = "pane_spawn.rs"]
pub mod pane_spawn;
#[path = "pane_spawn_runner.rs"]
pub mod pane_spawn_runner;
#[path = "runner.rs"]
pub mod runner;
#[path = "session_spawn.rs"]
pub mod session_spawn;
#[path = "stale_session_sweep.rs"]
pub mod stale_session_sweep;
#[path = "tmux_utils.rs"]
pub mod tmux_utils;
#[path = "window_spawn.rs"]
pub mod window_spawn;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "common/mod.rs"]
pub mod common;
