//! Port of `packages/coding-agent/src/core/tools/machine.ts`.

pub use crate::tool_machine::{ToolMachine, ToolPathPolicy, bind_tool_machine};
use maho_machine_ops::process::in_process_holder::create_in_process_holder_link;
use maho_machine_ops::{LocalMachineOpsOptions, MachineOps, create_local_machine_ops};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

static PROCESS_MACHINE_OPS: OnceLock<Arc<dyn MachineOps>> = OnceLock::new();
static DEFAULT_MACHINE: OnceLock<ToolMachine> = OnceLock::new();

/// The process-wide local `MachineOps` the built-in tools use when no caller supplies one.
/// Has no `state_dir`, so it records no scope tokens and writes nothing to disk.
pub fn default_local_machine_ops() -> Arc<dyn MachineOps> {
    Arc::clone(PROCESS_MACHINE_OPS.get_or_init(|| {
        create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: None,
            holder: create_in_process_holder_link(),
            limits: None,
            kill_process_tree: Some(Arc::new(|pid| {
                if let Ok(pid) = i32::try_from(pid) {
                    crate::shell::kill_process_tree(pid);
                }
            })),
            resolve_fd: Some(Arc::new(|| {
                Box::pin(async {
                    crate::managed_tools::ensure_tool(crate::managed_tools::ManagedTool::Fd)
                        .await
                        .map(PathBuf::from)
                })
            })),
        })
    }))
}

/// A `ToolMachine` over `ops`, or over the process-wide local machine when the caller supplies none.
pub fn create_tool_machine(
    ops: Option<Arc<dyn MachineOps>>,
    policy: Option<ToolPathPolicy>,
) -> ToolMachine {
    bind_tool_machine(
        Arc::new(move || ops.clone().unwrap_or_else(default_local_machine_ops)),
        policy,
    )
}

/// The `ToolMachine` a tool factory uses when its options carry none.
pub fn default_tool_machine() -> ToolMachine {
    DEFAULT_MACHINE
        .get_or_init(|| create_tool_machine(None, None))
        .clone()
}
