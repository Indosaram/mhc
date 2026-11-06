//! Drops macOS's stack-logging refusal from mhc's child stderr.
//!
//! Recent macOS prints one line per process when a system library stops Apple's private
//! `MallocStackLogging` recorder that was never started. Unrelated hosts (`bun`, `rsession`) print
//! it too and mhc never calls that framework, so short-lived session workers used to write this
//! over the TUI's alternate screen once per start:
//!
//! ```text
//! mhc(97489) MallocStackLogging: can't turn off malloc stack logging because it was not enabled.
//! ```

/// The refusal text, matched without its `<process>(<pid>)` prefix and line ending.
const STACK_LOGGING_REFUSAL: &[u8] =
    b"MallocStackLogging: can't turn off malloc stack logging because it was not enabled.";

/// Whether one stderr line is the stack-logging refusal above.
pub fn is_stack_logging_refusal(line: &[u8]) -> bool {
    line.windows(STACK_LOGGING_REFUSAL.len())
        .any(|window| window == STACK_LOGGING_REFUSAL)
}

/// Pipes a child's stderr for [`Self::start`] when a runtime can own the forwarding task.
///
/// Stdio is decided before `spawn`: piping without a reader would block a child on a full pipe, so
/// inheriting stays the fallback when no runtime is available.
pub struct ChildStderrRelay {
    handle: Option<tokio::runtime::Handle>,
}

impl ChildStderrRelay {
    /// Pipes `command`'s stderr for the relay, or leaves it inherited when no runtime can forward it.
    pub fn attach(command: &mut tokio::process::Command) -> Self {
        let handle = tokio::runtime::Handle::try_current().ok();
        if handle.is_some() {
            command.stderr(std::process::Stdio::piped());
        }
        Self { handle }
    }

    /// Starts forwarding the spawned child's stderr; a no-op unless [`Self::attach`] piped it.
    pub fn start(self, child: &mut tokio::process::Child) {
        let Some(handle) = self.handle else {
            return;
        };
        let Some(stderr) = child.stderr.take() else {
            return;
        };
        relay(handle, stderr);
    }
}

/// Forwards `stderr` through mhc's diagnostic path, dropping the stack-logging refusal.
///
/// Bytes go through [`maho_core::output_guard::maho_write_stderr`] like every other interactive
/// diagnostic: while the TUI owns the terminal they land in the debug log instead of the
/// alternate screen, and elsewhere they reach stderr unchanged.
pub fn relay(handle: tokio::runtime::Handle, stderr: tokio::process::ChildStderr) {
    handle.spawn(async move {
        forward(stderr, |text| {
            maho_core::output_guard::maho_write_stderr(text);
        })
        .await;
    });
}

/// Forwards `reader` line by line to `write`, skipping the stack-logging refusal.
async fn forward<R, F>(reader: R, mut write: F)
where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(&str),
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match lines.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if !is_stack_logging_refusal(&line) {
                    write(&String::from_utf8_lossy(&line));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{forward, is_stack_logging_refusal};

    #[test]
    fn drops_the_refusal_with_any_process_prefix_and_ending() {
        for line in [
            "mhc(97489) MallocStackLogging: can't turn off malloc stack logging because it was not enabled.\n",
            "mhc(97489) MallocStackLogging: can't turn off malloc stack logging because it was not enabled.\r\n",
            "MallocStackLogging: can't turn off malloc stack logging because it was not enabled.",
        ] {
            assert!(is_stack_logging_refusal(line.as_bytes()), "{line:?}");
        }
    }

    #[test]
    fn keeps_every_other_stderr_line() {
        for line in [
            "",
            "\n",
            "mhc(97489) some other message\n",
            "thread 'main' panicked at src/main.rs:1:1\n",
            // The framework's other diagnostics are real and must stay visible.
            "bun(71790) MallocStackLogging: recording malloc and VM allocation stacks to disk\n",
            "mhc(97489) MallocStackLogging: could not tag MSL-related memory as no_footprint\n",
        ] {
            assert!(!is_stack_logging_refusal(line.as_bytes()), "{line:?}");
        }
    }

    #[tokio::test]
    async fn forwards_real_stream_lines_and_skips_only_the_refusal() {
        let stream = concat!(
            "mhc(97489) MallocStackLogging: can't turn off malloc stack logging because it was not enabled.\n",
            "worker starting\n",
            "mhc(97490) MallocStackLogging: can't turn off malloc stack logging because it was not enabled.\r\n",
            "worker stopped",
        );
        let mut forwarded = Vec::new();
        forward(stream.as_bytes(), |text| forwarded.push(text.to_owned())).await;
        assert_eq!(forwarded, vec!["worker starting\n", "worker stopped"]);
    }
}
