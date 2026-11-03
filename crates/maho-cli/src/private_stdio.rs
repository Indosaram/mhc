//! Keeps this process's stdin/stdout/stderr out of the processes it spawns unless a spawn asks for
//! them, on every OS.
//!
//! Unix gets this from `O_CLOEXEC`: a child only receives the descriptors its `Stdio` names. Windows
//! does not: std's `CreateProcessW` passes `bInheritHandles = TRUE` without a handle list, so a child
//! inherits EVERY inheritable handle its parent holds, and the std handles this process was given as
//! its own parent's pipe ends are inheritable. Left alone, every process mhc spawns - whatever
//! `Stdio` it was given, a detached daemon included - keeps a copy of mhc's stdin/stdout/stderr pipe
//! ends, so whoever reads mhc's stdout (a test, the multi-session host reading a session worker's
//! protocol stream) sees no end-of-file after mhc exits until the last such descendant exits too.
//!
//! Clearing the inherit flag on the originals changes nothing a spawn asks for: `Stdio::inherit()`
//! duplicates the std handle as a fresh inheritable handle for exactly the child being spawned.
//!
//! The one OS call this needs lives in `omo_utils::runtime` (the workspace's process-runtime helper
//! crate, whose lint table allows its Windows FFI); this crate forbids unsafe code and only calls
//! the safe wrapper.

/// Clears the inherit flag on this process's std handles (a no-op off Windows, where `O_CLOEXEC`
/// already gives this). Every handle is attempted; the first failure is returned.
pub use omo_utils::runtime::keep_std_handles_private;

#[cfg(all(test, windows))]
mod tests {
    use std::io::{BufRead, Write};

    /// Set in the re-executed child: it is the process whose stdout the test reads.
    const CHILD_ENV: &str = "MAHO_PRIVATE_STDIO_TEST_CHILD";
    /// This test's libtest name, which the parent re-executes with `--exact`.
    const TEST_NAME: &str = "private_stdio::tests::a_spawned_process_does_not_hold_the_parents_stdio_pipes";
    const STARTED_LINE: &str = "PRIVATE_STDIO_CHILD_STARTED";
    const GRANDCHILD_PREFIX: &str = "GRANDCHILD_PID=";
    /// Safety bound on each wait below; a regression holds the pipe for minutes, not this long.
    const BOUND: std::time::Duration = std::time::Duration::from_secs(60);

    /// What the parent's reader thread saw on the child's stdout.
    enum Seen {
        Grandchild(String),
        /// End of the stream, with every line read (the whole transcript on a failure).
        End(Vec<String>),
        Failed(String),
    }

    /// One marker line on stdout (what the parent parses) and stderr (the test log), flushed now.
    fn report(line: &str) -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{line}").and_then(|()| stdout.flush())?;
        eprintln!("[private_stdio child] {line}");
        Ok(())
    }

    /// A child that keeps its std handles private spawns a long-lived grandchild with null stdio
    /// and waits on it; the test ends the child once the grandchild runs, and reading the child's
    /// stdout reaches end-of-file as soon as the child is gone. Before the fix the grandchild
    /// inherited the child's stdout pipe and held it open.
    #[test]
    fn a_spawned_process_does_not_hold_the_parents_stdio_pipes() {
        if std::env::var_os(CHILD_ENV).is_some() {
            // Proves the re-executed binary matched this test before anything else can fail.
            report(STARTED_LINE).expect("write the started marker");
            super::keep_std_handles_private().expect("clear the std handles' inherit flag");
            // `ping -n 600` outlives every bound here by minutes and needs no fixture.
            let mut grandchild = std::process::Command::new("ping")
                .args(["-n", "600", "127.0.0.1"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn the long-lived grandchild");
            if let Err(error) = report(&format!("{GRANDCHILD_PREFIX}{}", grandchild.id())) {
                let cleanup = grandchild.kill().and_then(|()| grandchild.wait());
                panic!("reporting the grandchild failed: {error}; grandchild cleanup: {cleanup:?}");
            }
            // The parent ends this process while it waits here; returning means the grandchild
            // died first, which would let the stream end without proving anything.
            let status = grandchild.wait();
            panic!("the grandchild exited while the child still ran: {status:?}");
        }
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("re-execute the test as the child");
        let stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        // The reader reports the grandchild's pid when its line arrives, then the end of the stream
        // with the whole transcript.
        let (events, received) = std::sync::mpsc::channel::<Seen>();
        std::thread::spawn(move || {
            let mut transcript = Vec::new();
            for line in stdout.lines() {
                match line {
                    Ok(line) => {
                        if let Some(pid) = line.strip_prefix(GRANDCHILD_PREFIX) {
                            let _ = events.send(Seen::Grandchild(pid.to_owned()));
                        }
                        transcript.push(line);
                    }
                    Err(error) => {
                        let _ = events.send(Seen::Failed(format!("{error}; read so far: {transcript:?}")));
                        return;
                    }
                }
            }
            let _ = events.send(Seen::End(transcript));
        });
        let grandchild = match received.recv_timeout(BOUND) {
            Ok(Seen::Grandchild(pid)) => pid,
            // No grandchild line: report how the child ended and everything it printed.
            other => {
                let killed = child.kill();
                let status = child.wait();
                let seen = match other {
                    Ok(Seen::End(transcript)) => format!("stdout ended; transcript: {transcript:#?}"),
                    Ok(Seen::Failed(error)) => format!("reading stdout failed: {error}"),
                    Ok(Seen::Grandchild(_)) => unreachable!("matched above"),
                    Err(error) => format!("no line within the bound: {error}"),
                };
                panic!("the child never reported its grandchild (kill {killed:?}, child status {status:?}); {seen}");
            }
        };
        // End the child while its grandchild runs: only an inherited pipe handle can keep the
        // child's stdout open past this.
        let ended = child.kill();
        let eof = received.recv_timeout(BOUND);
        let status = child.wait().expect("reap the child");
        // Clean up the grandchild whatever happened, before asserting.
        let killed = std::process::Command::new("taskkill").args(["/F", "/PID", &grandchild]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
        assert!(ended.is_ok(), "the child was ended while its grandchild ran: {ended:?}; child status {status}");
        match eof {
            Ok(Seen::End(transcript)) => assert!(
                // libtest prints "test <name> ... " without a newline before the child's output.
                transcript.iter().any(|line| line.ends_with(STARTED_LINE)),
                "the stream that ended is the child's; transcript: {transcript:#?}"
            ),
            Ok(Seen::Failed(error)) => panic!("reading the child's stdout failed: {error}"),
            Ok(Seen::Grandchild(pid)) => panic!("the child reported a second grandchild {pid}"),
            Err(error) => panic!("the child's stdout did not reach end-of-file while its grandchild ran ({error}); child status {status}"),
        }
        assert!(killed.is_ok_and(|status| status.success()), "the grandchild was still running when the stream ended");
    }
}
