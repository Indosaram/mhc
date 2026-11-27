//! `machine_op` record family handler and runtime (`senpi` `modes/rpc/machine-op/` @ `v2026.11.19-5`).

pub mod answer_cache;
pub mod core_ops;
pub mod executor;
pub mod handler;
pub mod live_handle;
pub mod machine_terminals;
pub mod op_args;
pub mod op_registry;
pub mod real_path;
pub mod terminal_args;
pub mod terminal_input;
pub mod terminal_ops;
pub mod terminal_wait;
pub mod wire;

pub use handler::MachineOpHandler;
pub use wire::{MACHINE_OP_CAPABILITY, MachineOpCommand, is_machine_op_command};
