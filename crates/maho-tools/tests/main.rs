//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "bash_via_device.rs"]
mod bash_via_device;
#[path = "contracts.rs"]
mod contracts;
#[path = "edit_parity.rs"]
mod edit_parity;
#[path = "grep_parity.rs"]
mod grep_parity;
#[path = "policy.rs"]
mod policy;
#[path = "read_blocking.rs"]
mod read_blocking;
#[path = "read_images.rs"]
mod read_images;
#[path = "read_window.rs"]
mod read_window;
#[path = "sandbox.rs"]
mod sandbox;
#[path = "shell_parity.rs"]
mod shell_parity;
#[path = "tool_path_agent_dir.rs"]
mod tool_path_agent_dir;
#[path = "tool_path_before_op.rs"]
mod tool_path_before_op;
#[path = "tools.rs"]
mod tools;
#[path = "tools_core_fake_machine.rs"]
mod tools_core_fake_machine;
#[path = "tools_core_parity.rs"]
mod tools_core_parity;
#[path = "tools_core_posix_signals.rs"]
mod tools_core_posix_signals;
#[path = "tools_core_spill.rs"]
mod tools_core_spill;
