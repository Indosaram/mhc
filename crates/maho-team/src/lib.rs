//! Team runs and terminal panes for mhc's delegated tasks (MIT; see `PROVENANCE.md`).
//!
//! - [`team`]: a named set of member tasks on top of `maho-task` with a per-team parallel limit,
//!   follow-up messages (queued while a member is busy), wait/collect, stop and team updates to
//!   the lead session.
//! - [`tools::TeamAddon`]: the `team_*` tools and `/team`, registered with the task tools.
//! - [`panes::PaneObserver`]: a `maho_task::TaskObserver` that follows each delegated task in a
//!   tmux or cmux pane and closes it when the task settles; headless when no multiplexer is
//!   available.
//!
//! Behaviour spec: `docs/cleanroom/specs/team.md`.
pub mod mux;
pub mod panes;
pub mod settings;
pub mod team;
pub mod tools;

pub use mux::{CmuxBackend, PaneBackend, PaneHandle, PaneRequest, TmuxBackend, detect};
pub use panes::PaneObserver;
pub use settings::{MuxKind, PaneMode, PaneSettings, Placement, TeamLimits};
pub use team::{MemberSpec, Team, TeamFile, TeamStatus, UPDATE_MESSAGE_TYPE};
pub use tools::{TOOL_NAMES, TeamAddon};
