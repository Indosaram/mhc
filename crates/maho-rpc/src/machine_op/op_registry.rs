//! Port of `packages/coding-agent/src/modes/rpc/machine-op/op-registry.ts`.
//!
//! The ops the engine serves for the relay host (`senpi#6752`), each with the relay host's own
//! decoder and run step.

use crate::machine_op::core_ops::{self, CoreCall};
use crate::machine_op::op_args::{self, ArgsError};
use crate::machine_op::terminal_args;
use crate::machine_op::terminal_ops::{TerminalOps, TerminalRun};
use crate::machine_op::wire::{Args, OpOutcome};
use maho_machine_ops::{AbortSignal, MachineOpError, MachineOps, OpContext};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct OpCall<'a> {
    pub ctx: &'a OpContext,
    pub ops: &'a Arc<dyn MachineOps>,
    pub root: &'a str,
    pub confined: bool,
    pub env_overlay: &'a BTreeMap<String, String>,
    pub sender: &'a str,
    pub signal: &'a AbortSignal,
    pub terminal: &'a TerminalOps,
    pub pid_of: &'a (dyn Fn(&str) -> Option<u32> + Send + Sync),
}

impl<'a> OpCall<'a> {
    fn core_call(&self) -> CoreCall<'_> {
        CoreCall {
            ctx: self.ctx,
            ops: self.ops,
            root: self.root,
            confined: self.confined,
            env_overlay: self.env_overlay,
        }
    }

    fn terminal_run(&self) -> TerminalRun<'_> {
        TerminalRun {
            sender: self.sender,
            root: self.root,
            confined: self.confined,
            signal: self.signal,
            env_overlay: self.env_overlay,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServedOpKind {
    FsRead,
    FsReadRange,
    FsWrite,
    FsWriteRange,
    FsEdit,
    FsGlob,
    FsGrep,
    FsStat,
    FsList,
    Shell,
    ProcessInfo,
    MeshMessage,
    ExecCommand,
    WriteStdin,
    WaitForCommand,
    KillTerminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineOpEntry {
    pub kind: ServedOpKind,
    /// The argument naming a terminal handle: the op acts only on its sender's own handles.
    pub handle_arg: Option<&'static str>,
}

fn malformed(err: ArgsError) -> OpOutcome {
    OpOutcome::refused("malformed", err.0)
}

impl MachineOpEntry {
    pub fn decode(&self, args: &Args) -> Option<String> {
        let res: Result<(), ArgsError> = match self.kind {
            ServedOpKind::FsRead => op_args::read_args(args).map(|_| ()),
            ServedOpKind::FsReadRange => op_args::read_range_args(args).map(|_| ()),
            ServedOpKind::FsWrite => op_args::write_args(args).map(|_| ()),
            ServedOpKind::FsWriteRange => op_args::write_range_args(args).map(|_| ()),
            ServedOpKind::FsEdit => op_args::edit_args(args).map(|_| ()),
            ServedOpKind::FsGlob => op_args::glob_args(args).map(|_| ()),
            ServedOpKind::FsGrep => op_args::grep_args(args).map(|_| ()),
            ServedOpKind::FsStat => op_args::stat_args(args).map(|_| ()),
            ServedOpKind::FsList => op_args::list_args(args).map(|_| ()),
            ServedOpKind::Shell => op_args::shell_args(args).map(|_| ()),
            ServedOpKind::ProcessInfo => op_args::process_info_args(args).map(|_| ()),
            ServedOpKind::MeshMessage => op_args::mesh_message_args(args).map(|_| ()),
            ServedOpKind::ExecCommand => terminal_args::exec_command_args(args).map(|_| ()),
            ServedOpKind::WriteStdin => terminal_args::write_stdin_args(args).map(|_| ()),
            ServedOpKind::WaitForCommand => terminal_args::wait_for_command_args(args).map(|_| ()),
            ServedOpKind::KillTerminal => terminal_args::kill_terminal_args(args).map(|_| ()),
        };
        res.err().map(|e| e.0)
    }

    pub async fn run(&self, args: &Args, call: &OpCall<'_>) -> Result<OpOutcome, MachineOpError> {
        let core = call.core_call();
        let term = call.terminal_run();
        match self.kind {
            ServedOpKind::FsRead => match op_args::read_args(args) {
                Ok(req) => core_ops::run_read(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsReadRange => match op_args::read_range_args(args) {
                Ok(req) => core_ops::run_read(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsWrite => match op_args::write_args(args) {
                Ok(req) => core_ops::run_write(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsWriteRange => match op_args::write_range_args(args) {
                Ok(req) => core_ops::run_write(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsEdit => match op_args::edit_args(args) {
                Ok(req) => core_ops::run_edit(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsGlob => match op_args::glob_args(args) {
                Ok(req) => core_ops::run_glob(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsGrep => match op_args::grep_args(args) {
                Ok(req) => core_ops::run_grep(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsStat => match op_args::stat_args(args) {
                Ok(req) => core_ops::run_stat(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::FsList => match op_args::list_args(args) {
                Ok(req) => core_ops::run_list(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::Shell => match op_args::shell_args(args) {
                Ok(req) => core_ops::run_shell(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::ProcessInfo => match op_args::process_info_args(args) {
                Ok(req) => core_ops::run_process_info(req, &core, call.pid_of).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::MeshMessage => match op_args::mesh_message_args(args) {
                Ok(req) => core_ops::run_mesh_message(req, &core).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::ExecCommand => match terminal_args::exec_command_args(args) {
                Ok(req) => call.terminal.exec_command(req, &term).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::WriteStdin => match terminal_args::write_stdin_args(args) {
                Ok(req) => call.terminal.write_stdin(req, &term).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::WaitForCommand => match terminal_args::wait_for_command_args(args) {
                Ok(req) => call.terminal.wait_for_command(req, &term).await,
                Err(err) => Ok(malformed(err)),
            },
            ServedOpKind::KillTerminal => match terminal_args::kill_terminal_args(args) {
                Ok(req) => Ok(call.terminal.kill_terminal(req).await),
                Err(err) => Ok(malformed(err)),
            },
        }
    }
}

pub fn served_op(name: &str) -> Option<MachineOpEntry> {
    let (kind, handle_arg) = match name {
        "fs.read" => (ServedOpKind::FsRead, None),
        "fs.read_range" => (ServedOpKind::FsReadRange, None),
        "fs.write" => (ServedOpKind::FsWrite, None),
        "fs.write_range" => (ServedOpKind::FsWriteRange, None),
        "fs.edit" => (ServedOpKind::FsEdit, None),
        "fs.glob" => (ServedOpKind::FsGlob, None),
        "fs.grep" => (ServedOpKind::FsGrep, None),
        "fs.stat" => (ServedOpKind::FsStat, None),
        "fs.list" => (ServedOpKind::FsList, None),
        "shell" => (ServedOpKind::Shell, None),
        "process.info" => (ServedOpKind::ProcessInfo, Some("handleId")),
        "mesh.message" => (ServedOpKind::MeshMessage, None),
        "exec_command" => (ServedOpKind::ExecCommand, None),
        "write_stdin" => (ServedOpKind::WriteStdin, Some("handle_id")),
        "wait_for_command" => (ServedOpKind::WaitForCommand, Some("handle_id")),
        "kill_terminal" => (ServedOpKind::KillTerminal, Some("handle_id")),
        _ => return None,
    };
    Some(MachineOpEntry { kind, handle_arg })
}
