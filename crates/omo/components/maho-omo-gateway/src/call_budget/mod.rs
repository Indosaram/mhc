//! Call budget utilities (`#14130`).
//!
//! Upstream counterpart: `packages/omo-senpi/src/components/call-budget/`.

pub mod call_deadline;

pub use call_deadline::{
    READ_RESERVE_MS, TOOL_CALL_BUDGET_MS, command_wait_ms, outside_call_deadline,
    wait_within_call_budget, with_call_deadline, within_call_budget,
};
