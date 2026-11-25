//! Port of `packages/machine-ops/src/process/wait-child.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::types::AbortSignal;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::sync::mpsc;

pub const EXIT_STDIO_GRACE_MS: u64 = 250;
pub const ABORT_EXIT_GRACE_MS: u64 = 5000;

#[derive(Clone, Default)]
pub struct WaitForChildProcessOptions {
    /// Abort when the caller has killed the process and abandoned its output.
    pub signal: Option<AbortSignal>,
    /// Post-abort deadline for `exit` before resolving anyway. Default 5s.
    pub abort_exit_grace_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct WaitedChildExit {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

pub fn extract_exit_signal(status: &ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig_num) = status.signal() {
            if let Ok(sig) = nix::sys::signal::Signal::try_from(sig_num) {
                return Some(sig.as_str().to_owned());
            }
            return Some(format!("SIG{sig_num}"));
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

pub fn extract_exit_code(status: &ExitStatus) -> Option<i32> {
    status.code()
}

pub(crate) enum StdioStream {
    Stdout,
    Stderr,
}

pub(crate) struct StdioChunk {
    pub stream: StdioStream,
    pub bytes: Vec<u8>,
}

/// Wait for a child process and its stdio streams without hanging on inherited stdio handles
/// from detached descendants (`pi#5303`).
pub(crate) async fn wait_child_with_stdio<F>(
    child: &mut Child,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    options: WaitForChildProcessOptions,
    mut on_chunk: F,
) -> Result<WaitedChildExit, MachineOpError>
where
    F: FnMut(StdioStream, &[u8]),
{
    let (tx, mut rx) = mpsc::unbounded_channel::<StdioChunk>();
    let mut stdout_ended = stdout.is_none();
    let mut stderr_ended = stderr.is_none();
    let (stdout_eof_tx, mut stdout_eof_rx) = tokio::sync::oneshot::channel::<()>();
    let (stderr_eof_tx, mut stderr_eof_rx) = tokio::sync::oneshot::channel::<()>();

    let stdout_task = match stdout {
        Some(mut out) => {
            let tx = tx.clone();
            Some(tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = tx.send(StdioChunk {
                                stream: StdioStream::Stdout,
                                bytes: buf[..n].to_vec(),
                            });
                        }
                    }
                }
                let _ = stdout_eof_tx.send(());
            }))
        }
        None => {
            let _ = stdout_eof_tx.send(());
            None
        }
    };

    let stderr_task = match stderr {
        Some(mut err_stream) => {
            let tx = tx.clone();
            Some(tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match err_stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = tx.send(StdioChunk {
                                stream: StdioStream::Stderr,
                                bytes: buf[..n].to_vec(),
                            });
                        }
                    }
                }
                let _ = stderr_eof_tx.send(());
            }))
        }
        None => {
            let _ = stderr_eof_tx.send(());
            None
        }
    };
    drop(tx);

    let abort_signal = options.signal;
    let abort_grace =
        Duration::from_millis(options.abort_exit_grace_ms.unwrap_or(ABORT_EXIT_GRACE_MS));
    let mut exited = false;
    let mut exit_result = WaitedChildExit {
        exit_code: None,
        signal: None,
    };
    let mut aborted_released = abort_signal.as_ref().is_some_and(AbortSignal::is_aborted);

    let mut post_exit_deadline: Option<tokio::time::Instant> = None;
    let mut abort_deadline: Option<tokio::time::Instant> =
        aborted_released.then(|| tokio::time::Instant::now() + abort_grace);
    let mut rx_closed = false;

    loop {
        if aborted_released && exited {
            break;
        }
        if exited && stdout_ended && stderr_ended {
            // Drain any chunks queued before EOF.
            while let Ok(chunk) = rx.try_recv() {
                on_chunk(chunk.stream, &chunk.bytes);
            }
            break;
        }

        tokio::select! {
            biased;

            maybe_chunk = rx.recv(), if !rx_closed => {
                match maybe_chunk {
                    Some(chunk) => {
                        on_chunk(chunk.stream, &chunk.bytes);
                        if exited && !aborted_released {
                            post_exit_deadline = Some(
                                tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS),
                            );
                        }
                    }
                    None => {
                        rx_closed = true;
                        stdout_ended = true;
                        stderr_ended = true;
                    }
                }
            }

            _ = &mut stdout_eof_rx, if !stdout_ended => {
                stdout_ended = true;
            }

            _ = &mut stderr_eof_rx, if !stderr_ended => {
                stderr_ended = true;
            }

            status_res = child.wait(), if !exited => {
                exited = true;
                match status_res {
                    Ok(status) => {
                        exit_result = WaitedChildExit {
                            exit_code: extract_exit_code(&status),
                            signal: extract_exit_signal(&status),
                        };
                    }
                    Err(err) => {
                        return Err(MachineOpError::from_io(&err, Some("wait"), None));
                    }
                }
                if aborted_released {
                    break;
                }
                if !(stdout_ended && stderr_ended) {
                    post_exit_deadline = Some(
                        tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS),
                    );
                }
            }

            () = async {
                if let Some(sig) = &abort_signal {
                    sig.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if !aborted_released && abort_signal.is_some() => {
                aborted_released = true;
                if let Some(h) = stdout_task.as_ref() {
                    h.abort();
                }
                if let Some(h) = stderr_task.as_ref() {
                    h.abort();
                }
                stdout_ended = true;
                stderr_ended = true;
                if exited {
                    break;
                }
                abort_deadline = Some(tokio::time::Instant::now() + abort_grace);
            }

            () = async {
                if let Some(dl) = post_exit_deadline {
                    tokio::time::sleep_until(dl).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if exited && !aborted_released && post_exit_deadline.is_some() => {
                // Grace period elapsed after exit without new output; abandon descendant-held pipes.
                while let Ok(chunk) = rx.try_recv() {
                    on_chunk(chunk.stream, &chunk.bytes);
                }
                break;
            }

            () = async {
                if let Some(dl) = abort_deadline {
                    tokio::time::sleep_until(dl).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if aborted_released && !exited && abort_deadline.is_some() => {
                return Ok(WaitedChildExit {
                    exit_code: None,
                    signal: None,
                });
            }
        }
    }

    if let Some(h) = stdout_task {
        h.abort();
    }
    if let Some(h) = stderr_task {
        h.abort();
    }
    Ok(exit_result)
}

/// Wait for a child process to terminate without hanging on inherited stdio handles.
pub async fn wait_for_child_process(
    child: &mut Child,
    options: Option<WaitForChildProcessOptions>,
) -> Result<Option<i32>, MachineOpError> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let waited = wait_child_with_stdio(
        child,
        stdout,
        stderr,
        options.unwrap_or_default(),
        |_, _| {},
    )
    .await
    .map_err(|err| MachineOpError::new(OpErrorCode::IoError, err.message))?;
    Ok(waited.exit_code)
}
