// `tests/support` is also compiled into the lib's unit tests and names the crate by its external path.
extern crate self as maho_omo_onboarding;

pub mod claim_owner;
pub mod component;
pub mod index;
pub mod scan_component;
pub mod scan_contract;
pub mod scan_runtime;
pub mod session_history;
pub mod state;
pub mod state_rpc;
pub mod tui_memory_runtime;
pub mod tui_memory_tools;
pub mod tui_scan_runtime;
pub mod understand_component;
pub mod understand_contract;
pub mod understand_control;
pub mod understand_runtime;
pub mod understand_start;
pub mod understand_step;
pub use index::*;
