//! Pane, window and session lifecycle plus layout and command-string helpers.

mod deps;
mod environment;
mod layout;
mod pane_activate;
mod pane_close;
mod pane_command;
mod pane_dimensions;
mod pane_replace;
mod pane_spawn;
mod server_health;
mod session_kill;
mod session_spawn;
mod stale_session_sweep;
mod window_spawn;

pub use deps::{
    ActivateTmuxPaneDeps, CloseTmuxPaneDependencies, DelayFn, EnforceMainPaneWidthDeps,
    GetPaneDimensionsDeps, GetTmuxPathFn, KillTmuxSessionDeps, LogFn, ReplaceTmuxPaneDeps,
    RunTmuxCommandFn, SpawnTmuxPaneDeps, SpawnTmuxSessionDeps, SpawnTmuxWindowDeps, TmuxDeps,
};
pub use environment::{
    SplitDirection, get_current_pane_id, is_inside_tmux, is_inside_tmux_environment,
    is_native_tmux, is_native_tmux_environment, is_tmux_pane_compatible,
    is_tmux_pane_compatible_environment,
};
pub use layout::{
    LayoutDeps, MainPaneWidthOptions, SpawnCommandFn, apply_layout, enforce_main_pane_width,
};
pub use pane_activate::{ActivateTmuxPaneRequest, activate_tmux_pane};
pub use pane_close::{close_tmux_pane, close_tmux_pane_with_dependencies};
pub use pane_command::{
    build_pane_auth_environment_args, build_pane_auth_environment_args_from,
    build_tmux_attach_command, build_tmux_placeholder_command,
};
pub use pane_dimensions::{PaneDimensions, get_pane_dimensions};
pub use pane_replace::{ReplaceTmuxPaneRequest, replace_tmux_pane};
pub use pane_spawn::{SpawnTmuxPaneRequest, spawn_tmux_pane};
pub use server_health::{
    FetchFn, IsServerRunningOptions, ServerHealthState, create_server_health_state,
    create_server_health_state_for_testing, is_server_marked_running_in_process, is_server_running,
    mark_server_running_in_process, reset_server_check,
};
pub use session_kill::kill_tmux_session_if_exists;
pub use session_spawn::{SpawnTmuxSessionRequest, get_isolated_session_name, spawn_tmux_session};
pub use stale_session_sweep::{
    SweepDeps, SweepTmuxSessionsDeps, SweepTmuxSessionsOptions, sweep_stale_omo_agent_sessions,
    sweep_stale_omo_agent_sessions_with, sweep_tmux_sessions_with,
};
pub use utils::runtime::spawn;
pub use window_spawn::{SpawnTmuxWindowRequest, spawn_tmux_window};
