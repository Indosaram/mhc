#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;
use utils::{AbortController, AbortSignal, ProcessHandle, SpawnOptions, SpawnedProcess, StdioMode, spawn};

const BOUND: Duration = Duration::from_secs(5);

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

struct ChildFixture {
    process: Option<SpawnedProcess>,
    handle: ProcessHandle,
    lines: Receiver<String>,
    reader_done: Receiver<()>,
    reader: Option<JoinHandle<()>>,
    exit: Option<Receiver<std::io::Result<i32>>>,
    waiter: Option<JoinHandle<()>>,
}

impl ChildFixture {
    fn new(script: &str, signal: Option<AbortSignal>) -> Self {
        let options = SpawnOptions {
            stdin: Some(StdioMode::Pipe),
            stdout: Some(StdioMode::Pipe),
            stderr: Some(StdioMode::Ignore),
            signal,
            ..Default::default()
        };
        let mut process = spawn(&sh(script), &options).unwrap_or_else(|error| panic!("{error}"));
        let handle = match process.handle() {
            Ok(handle) => handle,
            Err(error) => {
                process.kill();
                let exit = process.exited();
                panic!("process handle failed: {error}; cleanup: {exit:?}");
            }
        };
        let stdout = process.stdout.take().unwrap_or_else(|| panic!("stdout missing"));
        let (sender, lines) = mpsc::channel();
        let (done, reader_done) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => { if sender.send(line).is_err() { break; } }
                    Err(_) => break,
                }
            }
            let _ = done.send(());
        });
        Self { process: Some(process), handle, lines, reader_done, reader: Some(reader), exit: None, waiter: None }
    }

    fn line(&self) -> String {
        self.lines.recv_timeout(BOUND).unwrap_or_else(|error| panic!("child line: {error}"))
    }

    fn begin_wait(&mut self) {
        let mut process = self.process.take().unwrap_or_else(|| panic!("child already waiting"));
        let (sender, receiver) = mpsc::channel();
        self.exit = Some(receiver);
        self.waiter = Some(std::thread::spawn(move || {
            let _ = sender.send(process.exited());
        }));
    }

    fn await_exit(&mut self) -> i32 {
        let outcome = self.exit.as_ref().unwrap_or_else(|| panic!("wait not armed"))
            .recv_timeout(BOUND).unwrap_or_else(|error| panic!("child exit: {error}"));
        self.exit = None;
        if let Some(waiter) = self.waiter.take() {
            waiter.join().unwrap_or_else(|_| panic!("waiter panicked"));
        }
        outcome.unwrap_or_else(|error| panic!("{error}"))
    }
}

impl Drop for ChildFixture {
    fn drop(&mut self) {
        if self.process.is_some() || self.exit.is_some() {
            let killed = self.handle.kill();
            if self.process.is_some() { self.begin_wait(); }
            if let Some(exit) = self.exit.take() {
                match exit.recv_timeout(BOUND) {
                    Ok(Ok(_)) => {
                        if let Some(waiter) = self.waiter.take()
                            && waiter.join().is_err()
                        {
                            eprintln!("spawn fixture waiter panicked");
                        }
                    }
                    outcome => eprintln!("spawn fixture cleanup unsettled: {outcome:?}; kill: {killed:?}"),
                }
            }
        }
        if self.reader.is_some() {
            match self.reader_done.recv_timeout(BOUND) {
                Ok(()) => {
                    if let Some(reader) = self.reader.take()
                        && reader.join().is_err()
                    {
                        eprintln!("spawn fixture reader panicked");
                    }
                }
                outcome => eprintln!("spawn fixture reader cleanup unsettled: {outcome:?}"),
            }
        }
    }
}

#[test]
fn abort_terminates_a_blocked_child_without_explicit_kill() {
    let controller = AbortController::new();
    let mut child = ChildFixture::new("echo ready; while :; do read _; done", Some(controller.signal()));
    assert_eq!(child.line(), "ready");
    controller.abort();
    child.begin_wait();
    assert_eq!(child.await_exit(), 1);
}

#[test]
fn abort_cancels_a_child_while_exited_owns_the_wait() {
    let controller = AbortController::new();
    let mut child = ChildFixture::new("echo ready; read _; echo gate-released; while :; do read _; done", Some(controller.signal()));
    assert_eq!(child.line(), "ready");
    child.begin_wait();
    assert_eq!(child.line(), "gate-released");
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        controller.abort();
        let _ = sender.send(());
    });
    receiver.recv_timeout(BOUND).unwrap_or_else(|error| panic!("abort blocked: {error}"));
    worker.join().unwrap_or_else(|_| panic!("abort worker panicked"));
    assert_eq!(child.await_exit(), 1);
}

#[test]
fn a_shared_signal_cancels_each_owned_child() {
    let controller = AbortController::new();
    let mut first = ChildFixture::new("echo first; while :; do read _; done", Some(controller.signal()));
    let mut second = ChildFixture::new("echo second; while :; do read _; done", Some(controller.signal()));
    assert_eq!(first.line(), "first");
    assert_eq!(second.line(), "second");
    controller.abort();
    first.begin_wait();
    second.begin_wait();
    assert_eq!(first.await_exit(), 1);
    assert_eq!(second.await_exit(), 1);
}

#[test]
fn an_already_aborted_signal_terminates_the_spawned_child() {
    let controller = AbortController::new();
    controller.abort();
    let mut child = ChildFixture::new("while :; do read _; done", Some(controller.signal()));
    assert!(child.process.as_ref().is_some_and(|process| process.pid() > 0));
    child.begin_wait();
    assert_eq!(child.await_exit(), 1);
}

#[test]
fn a_later_abort_preserves_repeated_cached_exit() {
    let controller = AbortController::new();
    let mut child = ChildFixture::new("echo done", Some(controller.signal()));
    assert_eq!(child.line(), "done");
    let mut process = child.process.take().unwrap_or_else(|| panic!("child missing"));
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let first = process.exited();
        let _ = sender.send((first, process));
    });
    let (first, mut process) = match receiver.recv_timeout(BOUND) {
        Ok(outcome) => outcome,
        Err(error) => {
            let killed = child.handle.kill();
            let cleanup = receiver.recv_timeout(BOUND);
            if cleanup.is_ok() { worker.join().unwrap_or_else(|_| panic!("waiter panicked")); }
            panic!("exit wait failed: {error}; kill: {killed:?}; cleanup settled: {}", cleanup.is_ok());
        }
    };
    worker.join().unwrap_or_else(|_| panic!("waiter panicked"));
    controller.abort();
    let second = process.exited();
    assert_eq!((first.ok(), second.ok(), process.exit_code()), (Some(0), Some(0), Some(0)));
}

#[test]
fn explicit_kill_cleans_a_child_that_traps_abort_sigterm() {
    let controller = AbortController::new();
    let mut child = ChildFixture::new("trap 'echo term' TERM; echo ready; while :; do read _; done", Some(controller.signal()));
    assert_eq!(child.line(), "ready");
    controller.abort();
    assert_eq!(child.line(), "term");
    child.handle.kill().unwrap_or_else(|error| panic!("{error}"));
    child.begin_wait();
    assert_eq!(child.await_exit(), 1);
}
