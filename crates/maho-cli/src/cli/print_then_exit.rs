//! Port of senpi `cli/print-then-exit.ts` (pin 903b8336).
//!
//! A print-then-exit command (`--version`, `--help`, `--list-models`, `--export`) collects what it
//! prints and delivers it in one write before the process exits (senpi#2937). Upstream
//! patches `console.log`/`process.stdout.write`. Here `print` gets the collecting writer as its
//! argument. When stdout is taken over (`stdout_guard`, json mode sends it to stderr), the output
//! is left alone: `print` writes straight to stderr and nothing is collected.
//!
//! The delivery has the same two bounds as upstream: a reader that went away (`| head -1`,
//! EPIPE) ends the wait instead of failing the command, and a reader that never drains is given at
//! most `OUTPUT_DELIVERY_LIMIT_MS` before the process exits anyway.

use std::io::{self, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::stdout_guard;

/// The longest a print-then-exit command waits for its reader before exiting anyway.
pub const OUTPUT_DELIVERY_LIMIT_MS: u64 = 30_000;

/// senpi `captureStdout`: runs `print` with stdout collected instead of written. When `print`
/// fails, whatever it printed first is still written to stdout and the error is returned.
pub fn capture_stdout<T, E>(
    print: impl FnOnce(&mut dyn Write) -> Result<T, E>,
) -> Result<(T, String), E> {
    if stdout_guard::is_taken_over() {
        let mut stderr = io::stderr();
        return capture_into(
            Some(&mut stderr as &mut dyn Write),
            &mut io::stdout(),
            print,
        );
    }
    capture_into(None, &mut io::stdout(), print)
}

/// `redirected` is the writer an already-redirected stdout goes to; `stdout` receives what a
/// failed `print` collected.
fn capture_into<T, E>(
    redirected: Option<&mut dyn Write>,
    stdout: &mut dyn Write,
    print: impl FnOnce(&mut dyn Write) -> Result<T, E>,
) -> Result<(T, String), E> {
    if let Some(redirected) = redirected {
        return print(redirected).map(|result| (result, String::new()));
    }
    let mut chunks = Vec::new();
    match print(&mut chunks) {
        Ok(result) => Ok((result, String::from_utf8_lossy(&chunks).into_owned())),
        Err(error) => {
            if !chunks.is_empty() {
                // Best effort, as upstream's `finally` write: the print's own error is what the
                // caller reports.
                let _ = stdout.write_all(&chunks).and_then(|()| stdout.flush());
            }
            Err(error)
        }
    }
}

/// Writes `text` and flushes on a helper thread, returning when the write is done, the reader is
/// gone (any write error, EPIPE included), or `limit` has passed.
#[cfg(test)]
fn delivered<W: Write + Send + 'static>(stream: W, text: String, limit: Duration) {
    delivered_together(vec![(Box::new(stream), text)], limit);
}

/// senpi `Promise.all([delivered(stdout, ..), delivered(stderr, ..)])`: every stream is written
/// and flushed on its own helper thread at the same time, and the wait ends when all of them are
/// done or gone, or when the one shared `limit` has passed.
fn delivered_together(streams: Vec<(Box<dyn Write + Send>, String)>, limit: Duration) {
    let deadline = Instant::now() + limit;
    let (done, finished) = mpsc::channel();
    let count = streams.len();
    for (mut stream, text) in streams {
        let done = done.clone();
        std::thread::spawn(move || {
            let _ = stream
                .write_all(text.as_bytes())
                .and_then(|()| stream.flush());
            let _ = done.send(());
        });
    }
    for _ in 0..count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if finished.recv_timeout(remaining).is_err() {
            return;
        }
    }
}

/// senpi `exitAfterOutput`: delivers `output` to stdout and drains stderr together under one
/// limit, then exits with `code`.
pub fn exit_after_output(output: &str, code: i32) -> ! {
    delivered_together(
        vec![
            (Box::new(io::stdout()), output.to_owned()),
            (Box::new(io::stderr()), String::new()),
        ],
        Duration::from_millis(OUTPUT_DELIVERY_LIMIT_MS),
    );
    std::process::exit(code)
}

/// senpi `printThenExit`. A failing `print` is reported on stderr and exits 1, which is what an
/// uncaught rejection from upstream's `await printThenExit(...)` does.
pub fn print_then_exit<E: std::fmt::Display>(
    print: impl FnOnce(&mut dyn Write) -> Result<(), E>,
    code: i32,
) -> ! {
    match capture_stdout(print) {
        Ok(((), output)) => exit_after_output(&output, code),
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            exit_after_output("", 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn capture_collects_every_write_in_order() {
        let mut stdout = Vec::new();
        let (result, output) = capture_into(None, &mut stdout, |out| {
            writeln!(out, "header")?;
            for row in 0..3 {
                writeln!(out, "row {row}")?;
            }
            Ok::<_, io::Error>(7)
        })
        .expect("print");
        assert_eq!(result, 7);
        assert_eq!(output, "header\nrow 0\nrow 1\nrow 2\n");
        assert!(stdout.is_empty(), "nothing is written while capturing");
    }

    #[test]
    fn redirected_output_is_left_alone() {
        let (mut stderr, mut stdout) = (Vec::new(), Vec::new());
        let (_, output) = capture_into(Some(&mut stderr), &mut stdout, |out| {
            write!(out, "json line")?;
            Ok::<_, io::Error>(())
        })
        .expect("print");
        assert_eq!(output, "");
        assert_eq!(stderr, b"json line");
        assert!(stdout.is_empty());
    }

    #[test]
    fn a_failing_print_still_writes_what_it_printed_and_returns_its_error() {
        let mut stdout = Vec::new();
        let error = capture_into(None, &mut stdout, |out| {
            writeln!(out, "before the throw").map_err(|error| error.to_string())?;
            Err::<(), _>("boom".to_owned())
        })
        .expect_err("print fails");
        assert_eq!(error, "boom");
        assert_eq!(stdout, b"before the throw\n");
    }

    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_reader_that_went_away_ends_the_wait() {
        // A generous limit: the call can only return early because the write itself ended.
        delivered(Closed, "row\n".to_owned(), Duration::from_secs(3600));
    }

    #[derive(Clone)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_reader_receives_the_whole_output_before_the_wait_ends() {
        let sink = Shared(Arc::default());
        let text = format!("header\n{}", "row x\n".repeat(4000));
        delivered(sink.clone(), text.clone(), Duration::from_secs(3600));
        assert_eq!(*sink.0.lock().expect("lock"), text.into_bytes());
    }

    /// Blocks every write until `release` is dropped.
    struct Stalled(Mutex<mpsc::Receiver<()>>);
    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.lock().expect("lock").recv();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Waits for `peer` to finish before writing.
    struct AfterPeer(Mutex<mpsc::Receiver<()>>, Shared);
    impl Write for AfterPeer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.lock().expect("lock").recv();
            self.1.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Writes, then tells its peer it is done.
    struct ThenSignal(Mutex<Option<mpsc::Sender<()>>>, Shared);
    impl Write for ThenSignal {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let written = self.1.write(bytes)?;
            if let Some(peer) = self.0.lock().expect("lock").take() {
                let _ = peer.send(());
            }
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stdout_and_stderr_are_delivered_at_the_same_time() {
        // stdout can only finish once stderr was written. Delivered one after the other, stdout
        // would hold the wait for the whole (hour-long) limit; delivered together, both land.
        let (signal, wait) = mpsc::channel::<()>();
        let (out, err) = (Shared(Arc::default()), Shared(Arc::default()));
        delivered_together(
            vec![
                (
                    Box::new(AfterPeer(Mutex::new(wait), out.clone())),
                    "out".to_owned(),
                ),
                (
                    Box::new(ThenSignal(Mutex::new(Some(signal)), err.clone())),
                    "err".to_owned(),
                ),
            ],
            Duration::from_secs(3600),
        );
        assert_eq!(*out.0.lock().expect("lock"), b"out");
        assert_eq!(*err.0.lock().expect("lock"), b"err");
    }

    #[test]
    fn two_stalled_readers_share_one_limit() {
        let (release_out, stalled_out) = mpsc::channel::<()>();
        let (release_err, stalled_err) = mpsc::channel::<()>();
        delivered_together(
            vec![
                (
                    Box::new(Stalled(Mutex::new(stalled_out))),
                    "row\n".to_owned(),
                ),
                (Box::new(Stalled(Mutex::new(stalled_err))), String::new()),
            ],
            Duration::ZERO,
        );
        drop((release_out, release_err));
    }

    #[test]
    fn a_reader_that_never_drains_is_bounded_by_the_limit() {
        let (release, stalled) = mpsc::channel::<()>();
        delivered(
            Stalled(Mutex::new(stalled)),
            "row\n".to_owned(),
            Duration::ZERO,
        );
        drop(release);
    }
}
