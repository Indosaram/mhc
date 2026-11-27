//! Port of `packages/coding-agent/src/core/tools/machine-output-spill.ts`.

use crate::definition::{ToolError, ToolFuture};
use crate::fs_error_code::tool_error_from_machine;
use crate::output_accumulator_core::{
    CoreFullOutput, OutputSpill, OutputSpillFactory, join_head_and_tail,
};
use crate::tool_machine::ToolMachine;
use maho_machine_ops::{ReadRequest, WriteRequest};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

fn random_hex(byte_count: usize) -> String {
    let mut buf = vec![0u8; byte_count];
    let _ = getrandom::fill(&mut buf);
    let mut out = String::with_capacity(byte_count * 2);
    for b in buf {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

enum WriteOp {
    Chunk { offset: u64, bytes: Vec<u8> },
}

struct WriteWorkerOutcome {
    written: bool,
    error: Option<ToolError>,
}

struct MachineSpill {
    path: PathBuf,
    machine: ToolMachine,
    size: u64,
    tx: Option<mpsc::UnboundedSender<WriteOp>>,
    done_rx: Option<oneshot::Receiver<WriteWorkerOutcome>>,
    written: bool,
    latched_error: Option<ToolError>,
}

impl MachineSpill {
    fn new(path: PathBuf, machine: ToolMachine) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<WriteOp>();
        let (done_tx, done_rx) = oneshot::channel::<WriteWorkerOutcome>();
        let worker_path = path.to_string_lossy().into_owned();
        let worker_machine = machine.clone();

        tokio::spawn(async move {
            let mut written = false;
            let mut first_error: Option<ToolError> = None;
            let ctx = worker_machine.context(None, None);
            while let Some(op) = rx.recv().await {
                if first_error.is_some() {
                    continue;
                }
                match op {
                    WriteOp::Chunk { offset, bytes } => {
                        let res = worker_machine
                            .ops()
                            .files()
                            .write(
                                WriteRequest {
                                    path: worker_path.clone(),
                                    bytes,
                                    create_parent_dirs: true,
                                    offset: Some(offset),
                                    total: None,
                                },
                                &ctx,
                            )
                            .await;
                        match res {
                            Ok(_) => written = true,
                            Err(err) => first_error = Some(tool_error_from_machine(err)),
                        }
                    }
                }
            }
            let _ = done_tx.send(WriteWorkerOutcome {
                written,
                error: first_error,
            });
        });

        Self {
            path,
            machine,
            size: 0,
            tx: Some(tx),
            done_rx: Some(done_rx),
            written: false,
            latched_error: None,
        }
    }

    async fn settle_pending(&mut self) {
        self.tx.take();
        if let Some(rx) = self.done_rx.take()
            && let Ok(outcome) = rx.await
        {
            self.written |= outcome.written;
            if self.latched_error.is_none() {
                self.latched_error = outcome.error;
            }
        }
    }
}

impl OutputSpill for MachineSpill {
    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, chunk: &[u8]) {
        let offset = self.size;
        self.size += chunk.len() as u64;
        if let Some(tx) = &self.tx {
            let _ = tx.send(WriteOp::Chunk {
                offset,
                bytes: chunk.to_vec(),
            });
        }
    }

    fn close(&mut self) -> ToolFuture<'_, ()> {
        Box::pin(async move {
            self.settle_pending().await;
            if let Some(err) = self.latched_error.as_ref() {
                return Err(ToolError::Message(err.to_string()));
            }
            Ok(())
        })
    }

    fn remove(&mut self) -> ToolFuture<'_, ()> {
        Box::pin(async move {
            self.settle_pending().await;
            if !self.written {
                return Ok(());
            }
            let ctx = self.machine.context(None, None);
            let path_str = self.path.to_string_lossy().into_owned();
            self.machine
                .ops()
                .files()
                .write(
                    WriteRequest {
                        path: path_str,
                        bytes: Vec::new(),
                        create_parent_dirs: true,
                        offset: None,
                        total: None,
                    },
                    &ctx,
                )
                .await
                .map_err(tool_error_from_machine)?;
            Ok(())
        })
    }

    fn read_full(&mut self, max_bytes: usize) -> ToolFuture<'_, CoreFullOutput> {
        Box::pin(async move {
            let ctx = self.machine.context(None, None);
            let path_str = self.path.to_string_lossy().into_owned();
            let size_usize = usize::try_from(self.size).unwrap_or(usize::MAX);
            if size_usize <= max_bytes {
                let res = self
                    .machine
                    .ops()
                    .files()
                    .read(
                        ReadRequest {
                            path: path_str,
                            offset: Some(0),
                            limit_bytes: Some(size_usize),
                        },
                        &ctx,
                    )
                    .await
                    .map_err(tool_error_from_machine)?;
                return Ok(CoreFullOutput {
                    content: String::from_utf8_lossy(&res.bytes).into_owned(),
                    truncated: false,
                });
            }
            let head_bytes = max_bytes / 2;
            let tail_bytes = max_bytes - head_bytes;
            let tail_offset = self.size.saturating_sub(tail_bytes as u64);
            let ops = self.machine.ops();
            let (head_res, tail_res) = tokio::join!(
                ops.files().read(
                    ReadRequest {
                        path: path_str.clone(),
                        offset: Some(0),
                        limit_bytes: Some(head_bytes),
                    },
                    &ctx
                ),
                ops.files().read(
                    ReadRequest {
                        path: path_str,
                        offset: Some(tail_offset),
                        limit_bytes: Some(tail_bytes),
                    },
                    &ctx
                )
            );
            let head = head_res.map_err(tool_error_from_machine)?;
            let tail = tail_res.map_err(tool_error_from_machine)?;
            let omitted = self
                .size
                .saturating_sub(head_bytes as u64)
                .saturating_sub(tail_bytes as u64) as usize;
            Ok(CoreFullOutput {
                content: join_head_and_tail(&head.bytes, &tail.bytes, omitted),
                truncated: true,
            })
        })
    }
}

/// Spills to a file on `machine` under `temp_dir` (default `/tmp`), so the path a truncation notice
/// names is one the model can read on that machine.
pub fn create_machine_output_spill(
    machine: ToolMachine,
    temp_dir: Option<PathBuf>,
) -> OutputSpillFactory {
    let dir = temp_dir.unwrap_or_else(|| PathBuf::from("/tmp"));
    Arc::new(move |prefix| {
        let path = dir.join(format!("{prefix}-{}.log", random_hex(8)));
        Ok(Box::new(MachineSpill::new(path, machine.clone())))
    })
}
