//! The startup stub of a coordinated launch (senpi#4602, A-amended section 2).
//!
//! Captures `SENPI_RPC_HOST_STARTUP_EVENTS` once at initialization, removes it from the process
//! environment so nothing this process spawns later inherits it, emits the initial bootstrap
//! milestone if the startup pipe is open at `STARTUP_EVENT_FD`, and orders the supervisor's own
//! milestones against its host child's relayed records.

use crate::host_startup_wire::{
    MAX_STARTUP_EVENT_BYTES, STARTUP_EVENT_FD, StartupHandOff, StartupMilestone,
    StartupProducerRole, encode_startup_hand_off, parse_startup_hand_off, relay_startup_line,
    startup_event_line,
};
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const HOST_STARTUP_EVENTS_ENV: &str = "SENPI_RPC_HOST_STARTUP_EVENTS";

/// The supervisor's one writer to its startup pipe. Orders own milestones and relayed host-child
/// records so no host record is forwarded before `supervisor_setup`, and `internal_ready` (with
/// everything after it) waits for the relayed `host_runtime` unless the relay ended first.
pub struct StartupReporter {
    emit: Box<dyn FnMut(String) + Send>,
    relaying: bool,
    relay_ended: bool,
    setup_sent: bool,
    runtime_relayed: bool,
    held_own: Vec<String>,
    held_host: Vec<String>,
}

impl StartupReporter {
    pub fn new(emit: impl FnMut(String) + Send + 'static) -> Self {
        Self {
            emit: Box::new(emit),
            relaying: false,
            relay_ended: false,
            setup_sent: false,
            runtime_relayed: false,
            held_own: Vec::new(),
            held_host: Vec::new(),
        }
    }

    fn waiting_for_host(&self) -> bool {
        self.relaying && !self.relay_ended && !self.runtime_relayed
    }

    fn release_own(&mut self) {
        if !self.waiting_for_host() {
            for line in self.held_own.drain(..) {
                (self.emit)(line);
            }
        }
    }

    fn forward_host(&mut self, record: &str) {
        (self.emit)(relay_startup_line(record));
        if record_milestone(record).as_deref() == Some("host_runtime") {
            self.runtime_relayed = true;
        }
        self.release_own();
    }

    pub fn report(&mut self, line: String, milestone: StartupMilestone) {
        if !self.held_own.is_empty()
            || (milestone == StartupMilestone::InternalReady && self.waiting_for_host())
        {
            self.held_own.push(line);
            return;
        }
        (self.emit)(line);
        if milestone != StartupMilestone::SupervisorSetup {
            return;
        }
        self.setup_sent = true;
        let pending_host: Vec<String> = self.held_host.drain(..).collect();
        for record in pending_host {
            self.forward_host(&record);
        }
    }

    pub fn start_relay(&mut self) {
        self.relaying = true;
    }

    pub fn push_relayed_record(&mut self, record: String) {
        if self.setup_sent {
            self.forward_host(&record);
        } else {
            self.held_host.push(record);
        }
    }

    pub fn finish_relay(&mut self) {
        self.relay_ended = true;
        self.release_own();
    }

    /// Reads an async host startup stream and feeds each line (truncating over-limit lines so the
    /// launcher rejects them) into the shared reporter.
    pub fn relay_stream<R>(reporter: Arc<Mutex<Self>>, mut stream: R) -> tokio::task::JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        reporter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .start_relay();
        tokio::spawn(async move {
            let mut pending = String::new();
            let mut buf = [0u8; 4096];
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        pending.push_str(&String::from_utf8_lossy(&buf[..count]));
                        while let Some(newline) = pending.find('\n') {
                            let record = pending[..newline].to_owned();
                            pending.drain(..=newline);
                            reporter
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push_relayed_record(record);
                        }
                        if pending.len() > MAX_STARTUP_EVENT_BYTES {
                            let cut = pending
                                .char_indices()
                                .map(|(i, _)| i)
                                .find(|&i| i > MAX_STARTUP_EVENT_BYTES)
                                .unwrap_or(pending.len());
                            let truncated = pending[..cut].to_owned();
                            pending.clear();
                            reporter
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push_relayed_record(truncated);
                        }
                    }
                }
            }
            reporter
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finish_relay();
        })
    }
}

fn record_milestone(record: &str) -> Option<String> {
    let value: Value = serde_json::from_str(record).ok()?;
    value
        .as_object()?
        .get("milestone")?
        .as_str()
        .map(str::to_owned)
}

pub struct StartupBeacon {
    pub hand_off: StartupHandOff,
    seq: std::sync::atomic::AtomicU64,
    reporter: Arc<Mutex<StartupReporter>>,
}

impl StartupBeacon {
    pub fn new(
        hand_off: StartupHandOff,
        mut write_line: impl FnMut(String) -> bool + Send + 'static,
    ) -> Self {
        let seq = 1u64;
        let bootstrap = match hand_off.role {
            StartupProducerRole::Supervisor => StartupMilestone::SupervisorBootstrap,
            StartupProducerRole::Host => StartupMilestone::HostBootstrap,
        };
        let initial = startup_event_line(&hand_off.binding, hand_off.role, seq, bootstrap);
        let mut open = write_line(initial);
        let reporter = Arc::new(Mutex::new(StartupReporter::new(move |line| {
            if open && !write_line(line) {
                open = false;
            }
        })));
        Self {
            hand_off,
            seq: std::sync::atomic::AtomicU64::new(seq),
            reporter,
        }
    }

    pub fn report(&self, milestone: StartupMilestone) {
        let next_seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let line = startup_event_line(
            &self.hand_off.binding,
            self.hand_off.role,
            next_seq,
            milestone,
        );
        self.reporter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .report(line, milestone);
    }

    pub fn child_hand_off(&self, fillers: &[i32]) -> String {
        encode_startup_hand_off(&StartupHandOff {
            binding: self.hand_off.binding.clone(),
            role: StartupProducerRole::Host,
            budget_ms: self.hand_off.budget_ms,
            fillers: fillers.to_vec(),
        })
    }

    pub fn relay<R: AsyncRead + Unpin + Send + 'static>(
        &self,
        stream: R,
    ) -> tokio::task::JoinHandle<()> {
        StartupReporter::relay_stream(self.reporter.clone(), stream)
    }
}

#[cfg(unix)]
fn open_startup_pipe() -> Option<std::fs::File> {
    use std::os::unix::fs::FileTypeExt;
    let path = format!("/dev/fd/{STARTUP_EVENT_FD}");
    let meta = std::fs::metadata(&path).ok()?;
    let ft = meta.file_type();
    if !ft.is_fifo() && !ft.is_socket() {
        return None;
    }
    std::fs::OpenOptions::new().write(true).open(&path).ok()
}

#[cfg(not(unix))]
fn open_startup_pipe() -> Option<std::fs::File> {
    None
}

static BEACON: OnceLock<Option<StartupBeacon>> = OnceLock::new();

fn global_beacon() -> Option<&'static StartupBeacon> {
    BEACON
        .get_or_init(|| {
            let raw = std::env::var(HOST_STARTUP_EVENTS_ENV).ok();
            let hand_off = parse_startup_hand_off(raw.as_deref())?;
            let mut pipe = open_startup_pipe()?;
            Some(StartupBeacon::new(hand_off, move |line| {
                pipe.write_all(line.as_bytes()).is_ok()
            }))
        })
        .as_ref()
}

/// Initializes the process-wide startup beacon if `SENPI_RPC_HOST_STARTUP_EVENTS` is present and
/// `STARTUP_EVENT_FD` is open; returns the captured hand-off when coordinated.
pub fn init_startup_beacon() -> Option<StartupHandOff> {
    global_beacon().map(|b| b.hand_off.clone())
}

/// The hand-off this process reports for, when it is a coordinated producer.
pub fn startup_hand_off() -> Option<StartupHandOff> {
    global_beacon().map(|b| b.hand_off.clone())
}

pub fn report_startup_milestone(milestone: StartupMilestone) {
    if let Some(beacon) = global_beacon() {
        beacon.report(milestone);
    }
}

/// The environment value for the host child a coordinated supervisor spawns, or `None` when uncoordinated.
pub fn host_child_startup_hand_off(fillers: &[i32]) -> Option<String> {
    global_beacon().map(|beacon| beacon.child_hand_off(fillers))
}

/// The supervisor relays its host child's startup pipe verbatim inside relay frames; it never writes a host
/// milestone itself.
pub fn relay_host_startup_events<R: AsyncRead + Unpin + Send + 'static>(
    stream: Option<R>,
) -> Option<tokio::task::JoinHandle<()>> {
    let stream = stream?;
    if let Some(beacon) = global_beacon() {
        Some(beacon.relay(stream))
    } else {
        Some(tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_startup_wire::{StartupBinding, StartupBindingKind};
    use tokio::io::AsyncWriteExt;

    fn binding() -> StartupBinding {
        StartupBinding {
            attempt: "att".into(),
            endpoint: "/tmp/rpc.sock".into(),
            instance_id: "gen-1".into(),
            generation: 1,
            launch_kind: StartupBindingKind::Initial,
            predecessor: None,
        }
    }

    #[test]
    fn reporter_holds_host_before_setup_and_holds_internal_ready_until_host_runtime() {
        let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = emitted.clone();
        let mut reporter = StartupReporter::new(move |line| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line);
        });
        reporter.start_relay();

        let host_boot = startup_event_line(
            &binding(),
            StartupProducerRole::Host,
            1,
            StartupMilestone::HostBootstrap,
        );
        let host_rt = startup_event_line(
            &binding(),
            StartupProducerRole::Host,
            2,
            StartupMilestone::HostRuntime,
        );
        reporter.push_relayed_record(host_boot.trim_end().to_owned());
        // Not forwarded before supervisor_setup.
        assert!(
            emitted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );

        let setup = startup_event_line(
            &binding(),
            StartupProducerRole::Supervisor,
            2,
            StartupMilestone::SupervisorSetup,
        );
        reporter.report(setup, StartupMilestone::SupervisorSetup);
        assert_eq!(
            emitted.lock().unwrap_or_else(PoisonError::into_inner).len(),
            2
        );

        let internal = startup_event_line(
            &binding(),
            StartupProducerRole::Supervisor,
            3,
            StartupMilestone::InternalReady,
        );
        let observer = startup_event_line(
            &binding(),
            StartupProducerRole::Supervisor,
            4,
            StartupMilestone::ObserverReady,
        );
        reporter.report(internal, StartupMilestone::InternalReady);
        reporter.report(observer, StartupMilestone::ObserverReady);
        // Both held until host_runtime arrives.
        assert_eq!(
            emitted.lock().unwrap_or_else(PoisonError::into_inner).len(),
            2
        );

        reporter.push_relayed_record(host_rt.trim_end().to_owned());
        let lines = emitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(lines.len(), 5);
        assert!(lines[2].contains("host_runtime"));
        assert!(lines[3].contains("internal_ready"));
        assert!(lines[4].contains("observer_ready"));
    }

    #[tokio::test]
    async fn relay_stream_truncates_oversized_record_and_releases_on_eof() {
        let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = emitted.clone();
        let mut reporter = StartupReporter::new(move |line| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line);
        });
        reporter.report("setup\n".into(), StartupMilestone::SupervisorSetup);
        let shared = Arc::new(Mutex::new(reporter));
        let (mut writer, reader) = tokio::io::duplex(16_384);
        let handle = StartupReporter::relay_stream(shared.clone(), reader);

        shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .report("internal\n".into(), StartupMilestone::InternalReady);
        // Nothing emitted yet for internal_ready while relay is open.
        assert_eq!(
            emitted.lock().unwrap_or_else(PoisonError::into_inner).len(),
            1
        );

        let oversized = "a".repeat(MAX_STARTUP_EVENT_BYTES + 100);
        writer.write_all(oversized.as_bytes()).await.expect("write");
        drop(writer);
        handle.await.expect("join");

        let lines = emitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // setup, oversized relay frame, empty trailing fragment or internal_ready released on EOF.
        assert!(lines.iter().any(|l| l == "internal\n"));
        let frame: Value = serde_json::from_str(lines[1].trim_end()).expect("relay json");
        assert_eq!(
            frame["line"].as_str().map(str::len),
            Some(MAX_STARTUP_EVENT_BYTES + 1)
        );
    }

    #[test]
    fn beacon_emits_bootstrap_on_init_and_builds_host_handoff() {
        let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = emitted.clone();
        let hand_off = StartupHandOff {
            binding: binding(),
            role: StartupProducerRole::Supervisor,
            budget_ms: 120_000,
            fillers: vec![5],
        };
        let beacon = StartupBeacon::new(hand_off, move |line| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line);
            true
        });
        beacon.report(StartupMilestone::SupervisorSetup);
        let lines = emitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("supervisor_bootstrap"));
        assert!(lines[1].contains("supervisor_setup"));

        let child_encoded = beacon.child_hand_off(&[7, 8]);
        let child = parse_startup_hand_off(Some(&child_encoded)).expect("child handoff");
        assert_eq!(child.role, StartupProducerRole::Host);
        assert_eq!(child.fillers, vec![7, 8]);
    }
}
