//! The execution barrier inside a tool-child worker thread (senpi#3566, plan todo 4).
//!
//! A worker claims a slot on the shared barrier clock itself, asks the protocol loop (main thread) to send `launch`,
//! and blocks on the clock's ack wait until the loop records the server's ack in that slot. Only then does it store
//! the invoke tick and call the real API. Blocking the worker is what makes every API - synchronous ones included -
//! safe: nothing can run, and no caller can write to a shim's stdin, before the ack. The main thread stays free
//! throughout, so it can receive the ack and publish the proof even while the command itself blocks this worker.
use std::collections::HashMap;
use std::thread::ThreadId;

use crate::barrier_clock::{AckWait, BarrierClock};
use crate::launch_gate::LaunchRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerLaunchMessage {
    Launch {
        launch_id: String,
        slot: usize,
        request: LaunchRequest,
    },
    Invoking {
        launch_id: String,
    },
    Started {
        launch_id: String,
        pid: u32,
        breakaway: bool,
    },
    Done {
        launch_id: String,
        exit_code: Option<i32>,
        signal: Option<String>,
    },
}

/// `Omit<LaunchRequest, "jobId">`: the request a worker supplies; the client adds the job id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLaunchRequest {
    pub origin: crate::protocol::LaunchOrigin,
    pub api: crate::protocol::LaunchApi,
    pub containment: crate::protocol::LaunchContainment,
    pub lifetime: crate::protocol::LaunchLifetime,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
}

pub struct WorkerLaunchClientOptions {
    pub clock: BarrierClock,
    pub post: Box<dyn Fn(WorkerLaunchMessage) + Send + Sync>,
    pub job_id: Box<dyn Fn() -> String + Send + Sync>,
    /// Test-only `SKIP_ACK_WAIT` mutation.
    pub skip_ack_wait: bool,
    /// Bounded wait for the ack; the server kills the child long before this in practice. Default 60 000.
    pub ack_timeout_ms: Option<u64>,
    /// The worker's identity inside launch ids; the worker thread id by default.
    pub worker_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct WorkerLaunchError {
    pub code: String,
    pub message: String,
}

impl WorkerLaunchError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What a worker reports when its command finished; `None` fields are left out of the `done` message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerLaunchOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

fn thread_id_number(id: ThreadId) -> String {
    // `ThreadId` has no stable integer accessor; its Debug form is `ThreadId(N)`.
    let text = format!("{id:?}");
    text.trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .to_string()
}

pub struct WorkerLaunchClient {
    clock: BarrierClock,
    options: WorkerLaunchClientOptions,
    next: u64,
    slot_of: HashMap<String, usize>,
}

impl WorkerLaunchClient {
    pub fn new(options: WorkerLaunchClientOptions) -> Self {
        Self {
            clock: options.clock.clone(),
            options,
            next: 0,
            slot_of: HashMap::new(),
        }
    }

    /// Sends `launch` and blocks this worker until the ack; returns the launch id for `invoke`/`done`.
    pub fn acquire(&mut self, request: WorkerLaunchRequest) -> Result<String, WorkerLaunchError> {
        let slot = self.clock.claim_slot().ok_or_else(|| {
            WorkerLaunchError::new(
                "tool_child_launch_capacity",
                format!("{} launches in flight", self.clock.slots),
            )
        })?;
        // Worker threads share the pid and each job's worker starts its own counter: the thread id keeps ids unique
        // across every worker of a generation.
        self.next += 1;
        let worker_id = match &self.options.worker_id {
            Some(id) => id.clone(),
            None => thread_id_number(std::thread::current().id()),
        };
        let launch_id = format!(
            "W{}-{}-{}-{}",
            std::process::id(),
            worker_id,
            slot,
            self.next
        );
        (self.options.post)(WorkerLaunchMessage::Launch {
            launch_id: launch_id.clone(),
            slot,
            request: LaunchRequest {
                job_id: (self.options.job_id)(),
                origin: request.origin,
                api: request.api,
                containment: request.containment,
                lifetime: request.lifetime,
                pid: request.pid,
                pgid: request.pgid,
            },
        });
        if !self.options.skip_ack_wait
            && self
                .clock
                .wait_for_ack(
                    slot,
                    Some(std::time::Duration::from_millis(
                        self.options.ack_timeout_ms.unwrap_or(60_000),
                    )),
                )
                .map_or(true, |wait| wait == AckWait::TimedOut)
        {
            return Err(WorkerLaunchError::new(
                "tool_child_launch_unacknowledged",
                launch_id,
            ));
        }
        self.slot_of.insert(launch_id.clone(), slot);
        Ok(launch_id)
    }

    /// Immediately before the real call: store the invoke tick and let the protocol loop publish the proof.
    pub fn invoke(&mut self, launch_id: &str) -> Result<(), WorkerLaunchError> {
        let Some(slot) = self.slot_of.get(launch_id).copied() else {
            return Err(WorkerLaunchError::new(
                "tool_child_launch_unknown",
                launch_id,
            ));
        };
        let _ = self.clock.record_invoke(slot);
        (self.options.post)(WorkerLaunchMessage::Invoking {
            launch_id: launch_id.to_string(),
        });
        Ok(())
    }

    /// Windows: the command's pid once the real call returned it, for the server's creation-time membership record.
    pub fn started(&self, launch_id: &str, pid: u32, breakaway: bool) {
        (self.options.post)(WorkerLaunchMessage::Started {
            launch_id: launch_id.to_string(),
            pid,
            breakaway,
        });
    }

    pub fn done(&mut self, launch_id: &str, outcome: WorkerLaunchOutcome) {
        self.slot_of.remove(launch_id);
        (self.options.post)(WorkerLaunchMessage::Done {
            launch_id: launch_id.to_string(),
            exit_code: outcome.exit_code,
            signal: outcome.signal,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin};
    use std::sync::{Arc, Mutex};

    type Messages = Arc<Mutex<Vec<WorkerLaunchMessage>>>;

    fn client(
        skip_ack_wait: bool,
        ack_timeout_ms: Option<u64>,
        slots: usize,
    ) -> Option<(WorkerLaunchClient, Messages)> {
        let messages: Messages = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&messages);
        let clock = BarrierClock::create(slots).ok()?;
        let client = WorkerLaunchClient::new(WorkerLaunchClientOptions {
            clock,
            post: Box::new(move |message| {
                sink.lock().unwrap_or_else(|e| e.into_inner()).push(message)
            }),
            job_id: Box::new(|| "j1".to_string()),
            skip_ack_wait,
            ack_timeout_ms,
            worker_id: Some("7".to_string()),
        });
        Some((client, messages))
    }

    fn request() -> WorkerLaunchRequest {
        WorkerLaunchRequest {
            origin: LaunchOrigin::Tool,
            api: LaunchApi::CpSpawn,
            containment: LaunchContainment::OwnGroup,
            lifetime: LaunchLifetime::Owned,
            pid: None,
            pgid: None,
        }
    }

    #[test]
    fn acquire_posts_launch_with_the_job_id_and_a_unique_id() {
        let (mut client, messages) = client(true, None, 4).expect("clock");
        let first = client.acquire(request()).expect("first");
        let second = client.acquire(request()).expect("second");
        assert_ne!(first, second);
        assert!(first.starts_with(&format!("W{}-7-", std::process::id())));
        let posted = messages.lock().expect("lock");
        let WorkerLaunchMessage::Launch {
            launch_id, request, ..
        } = &posted[0]
        else {
            panic!("first message is not launch");
        };
        assert_eq!(launch_id, &first);
        assert_eq!(request.job_id, "j1");
        assert_eq!(request.containment, LaunchContainment::OwnGroup);
    }

    #[test]
    fn acquire_times_out_without_an_ack() {
        let (mut client, _) = client(false, Some(10), 1).expect("clock");
        let error = client.acquire(request()).expect_err("unacknowledged");
        assert_eq!(error.code, "tool_child_launch_unacknowledged");
        assert!(
            error
                .to_string()
                .starts_with("tool_child_launch_unacknowledged: W")
        );
    }

    #[test]
    fn acquire_reports_capacity_when_every_slot_is_in_flight() {
        let (mut client, _) = client(true, None, 1).expect("clock");
        client.acquire(request()).expect("first");
        let error = client.acquire(request()).expect_err("capacity");
        assert_eq!(
            error.to_string(),
            "tool_child_launch_capacity: 1 launches in flight"
        );
    }

    #[test]
    fn invoke_records_the_tick_and_posts_invoking_and_unknown_ids_fail() {
        let (mut client, messages) = client(true, None, 4).expect("clock");
        let launch_id = client.acquire(request()).expect("acquire");
        client.invoke(&launch_id).expect("invoke");
        assert_eq!(
            messages.lock().expect("lock").last(),
            Some(&WorkerLaunchMessage::Invoking {
                launch_id: launch_id.clone()
            })
        );
        let error = client.invoke("nope").expect_err("unknown");
        assert_eq!(error.to_string(), "tool_child_launch_unknown: nope");
    }

    #[test]
    fn started_and_done_post_their_messages_and_done_forgets_the_slot() {
        let (mut client, messages) = client(true, None, 4).expect("clock");
        let launch_id = client.acquire(request()).expect("acquire");
        client.started(&launch_id, 42, true);
        client.done(
            &launch_id,
            WorkerLaunchOutcome {
                exit_code: Some(3),
                signal: None,
            },
        );
        let posted = messages.lock().expect("lock");
        assert_eq!(
            posted[1],
            WorkerLaunchMessage::Started {
                launch_id: launch_id.clone(),
                pid: 42,
                breakaway: true
            }
        );
        assert_eq!(
            posted[2],
            WorkerLaunchMessage::Done {
                launch_id: launch_id.clone(),
                exit_code: Some(3),
                signal: None
            }
        );
        drop(posted);
        assert_eq!(
            client.invoke(&launch_id).expect_err("forgotten").code,
            "tool_child_launch_unknown"
        );
    }
}
