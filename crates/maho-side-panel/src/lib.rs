//! Read-only status side panel for the interactive TUI (docs/cleanroom/specs/side-panel.md).
//!
//! The panel never changes session state. Every view is derived from host events, task records
//! and `git`. The crate is split into pure, testable parts and a thin extension adapter:
//!
//! - [`settings`]: the `side_panel` config block, validation and the enable decision (spec 1-2).
//! - [`layout`]: effective width, column visibility, mount mode and the one-time notice (spec 3).
//! - [`text`]: cell widths, sanitizing, fitting rows to the panel width (spec 4).
//! - [`sections`]: the section data models and the composed panel rows (spec 4-5).
//! - [`tasks`]: the delegated-task board read from the task state directory (spec 5 Agents).
//! - [`tools`]: the recent tool-call log (spec 5 Tools).
//! - [`git`]: `git status`/numstat parsing, the throttled refresh and diff commands (spec 5-7).
//! - [`goal`]: the goal state file reader with its mtime/size cache (spec 5 Goal).
//! - [`links`]: the private `mhc-panel:` link scheme (spec 7).
//! - [`viewer`]: the framed read-only detail viewer (spec 7).
//! - [`panel`]: the panel controller (state, refresh lifecycle, actions; spec 6-7).
//! - [`extension`]: the `<builtin:side-panel>` extension wiring it into a session.

pub mod extension;
pub mod git;
pub mod goal;
pub mod layout;
pub mod links;
pub mod panel;
pub mod sections;
pub mod settings;
pub mod tasks;
pub mod text;
pub mod tools;
pub mod viewer;

pub use extension::{DIFF_COMMAND, FLAG_ALIAS, FLAG_NAME, SidePanelExtension};
pub use panel::{Panel, PanelHost};
pub use settings::{Settings, SettingsError};
