//! senpi `modes/rpc/tool-child/job-updates.ts` (pin 26acd9353).
//!
//! A child job's streamed updates (senpi#3566 todo 9): `update(value)` sends the job's next partial result in order,
//! by acknowledged reference when large, and waits for credit once `updatesInFlight` are unacknowledged - so a fast
//! producer is paced by the server instead of buffering without bound, and no update is ever dropped.
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tokio::sync::{Notify, oneshot};

use crate::host_calls_server::ChannelSend;
use crate::payload_ref::PayloadRefWriter;
use crate::payload_wire::{PackedPayload, pack_payload};
use crate::protocol::{ChildToServer, TOOL_CHILD_LIMITS, ToolChildMessage};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobUpdatesError {
    #[error("update after job {0} ended")]
    Ended(String),
    #[error("{0}")]
    Pack(String),
}

#[derive(Default)]
struct State {
    next_seq: u64,
    acked: u64,
    ended: bool,
    /// The previous update's completion: updates go out in call order, each packed before it takes its seq.
    tail: Option<oneshot::Receiver<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    job_id: String,
    channel: Arc<dyn ChannelSend>,
    writer: Option<Arc<PayloadRefWriter>>,
    window: u64,
    state: Mutex<State>,
    wake: Notify,
}

pub struct JobUpdates {
    inner: Arc<Inner>,
}

pub struct JobUpdatesOptions {
    pub job_id: String,
    pub channel: Arc<dyn ChannelSend>,
    pub writer: Option<Arc<PayloadRefWriter>>,
    /// Defaults to `TOOL_CHILD_LIMITS.updates_in_flight`.
    pub window: Option<u64>,
}

impl JobUpdates {
    pub fn new(options: JobUpdatesOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                job_id: options.job_id,
                channel: options.channel,
                writer: options.writer,
                window: options
                    .window
                    .unwrap_or(TOOL_CHILD_LIMITS.updates_in_flight as u64),
                state: Mutex::new(State::default()),
                wake: Notify::new(),
            }),
        }
    }

    /// Sends the next update once a credit is free, after every earlier one; fails after the job ended.
    ///
    /// Like upstream's promise chain, the send starts at the call (on the current tokio runtime), so an update whose
    /// future is not awaited still goes out, in call order.
    pub fn update(
        &self,
        value: Value,
    ) -> impl Future<Output = Result<(), JobUpdatesError>> + Send + use<> {
        let (done, next_tail) = oneshot::channel();
        let previous = lock(&self.inner.state).tail.replace(next_tail);
        let inner = self.inner.clone();
        let sent = tokio::spawn(async move {
            if let Some(previous) = previous {
                // Resolves when the earlier update was sent (or failed); its sender drops either way.
                let _ = previous.await;
            }
            let outcome = send(&inner, value).await;
            let _ = done.send(());
            outcome
        });
        async move {
            sent.await
                .unwrap_or_else(|error| Err(JobUpdatesError::Pack(error.to_string())))
        }
    }

    /// The server delivered update `seq`: every update up to it holds no credit any more.
    pub fn acked(&self, seq: u64) {
        {
            let mut state = lock(&self.inner.state);
            if seq <= state.acked {
                return;
            }
            state.acked = seq.min(state.next_seq);
        }
        self.inner.wake.notify_waiters();
    }

    /// The job ended: waiting producers are released and further updates refused.
    pub fn end(&self) {
        lock(&self.inner.state).ended = true;
        self.inner.wake.notify_waiters();
    }
}

async fn send(inner: &Inner, value: Value) -> Result<(), JobUpdatesError> {
    let ended = || JobUpdatesError::Ended(inner.job_id.clone());
    loop {
        let notified = inner.wake.notified();
        tokio::pin!(notified);
        // Registered before the check, so an `acked` / `end` between the check and the wait is not lost.
        notified.as_mut().enable();
        {
            let state = lock(&inner.state);
            if state.ended || state.next_seq - state.acked < inner.window {
                break;
            }
        }
        notified.await;
    }
    if lock(&inner.state).ended {
        return Err(ended());
    }
    let packed = match &inner.writer {
        None => PackedPayload::Inline(value),
        Some(writer) => pack_payload(&value, writer)
            .await
            .map_err(|error| JobUpdatesError::Pack(error.to_string()))?,
    };
    let seq = {
        let mut state = lock(&inner.state);
        if state.ended {
            return Err(ended());
        }
        state.next_seq += 1;
        state.next_seq
    };
    let job_id = inner.job_id.clone();
    inner.channel.send(match packed {
        PackedPayload::Inline(value) => ToolChildMessage::Child(ChildToServer::Update {
            job_id,
            seq,
            value: Some(value),
            value_ref: None,
        }),
        PackedPayload::Ref(value_ref) => ToolChildMessage::Child(ChildToServer::Update {
            job_id,
            seq,
            value: None,
            value_ref: Some(value_ref),
        }),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload_ref::PayloadRefWriterOptions;
    use serde_json::json;

    // senpi#3566 todo 9: a child job streams partial results to the server in order, paced by acknowledgement (at most
    // TOOL_CHILD_LIMITS.updatesInFlight unacknowledged), large ones by reference, and its result settles after every
    // update. (The tests against a real tool child live with the client.)

    #[derive(Default)]
    struct UpdateRecorder {
        frames: Mutex<Vec<(u64, bool)>>,
        sent: Notify,
    }

    impl ChannelSend for UpdateRecorder {
        fn send(&self, message: ToolChildMessage) -> bool {
            if let ToolChildMessage::Child(ChildToServer::Update { seq, value_ref, .. }) = message {
                lock(&self.frames).push((seq, value_ref.is_some()));
                self.sent.notify_waiters();
            }
            true
        }
    }

    impl UpdateRecorder {
        fn seqs(&self) -> Vec<u64> {
            lock(&self.frames).iter().map(|(seq, _)| *seq).collect()
        }

        /// Waits until `count` updates went out, bounded.
        async fn until_sent(&self, count: usize) {
            let wait = async {
                loop {
                    let notified = self.sent.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if lock(&self.frames).len() >= count {
                        return;
                    }
                    notified.await;
                }
            };
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(10), wait)
                    .await
                    .is_ok(),
                "timed out waiting for {count} updates"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn paces_a_producer_at_the_window_exactly_the_window_goes_out_unacknowledged_each_ack_frees_one()
     {
        let recorder = Arc::new(UpdateRecorder::default());
        let stream = Arc::new(JobUpdates::new(JobUpdatesOptions {
            job_id: "j1".to_owned(),
            channel: recorder.clone(),
            writer: None,
            window: Some(4),
        }));
        let producer = {
            let stream = stream.clone();
            tokio::spawn(async move {
                for index in 0..10 {
                    stream
                        .update(json!(index))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            })
        };
        recorder.until_sent(4).await;
        assert_eq!(recorder.seqs(), vec![1, 2, 3, 4]);
        stream.acked(2);
        recorder.until_sent(6).await;
        assert_eq!(recorder.seqs(), vec![1, 2, 3, 4, 5, 6]);
        stream.acked(10);
        assert_eq!(producer.await.ok(), Some(Ok(())));
        assert_eq!(recorder.seqs(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sends_unawaited_updates_in_call_order_with_consecutive_seqs_a_large_one_packed_first_included()
     {
        let Ok(root) = tempfile::tempdir() else {
            panic!("a temp dir");
        };
        let recorder = Arc::new(UpdateRecorder::default());
        let stream = JobUpdates::new(JobUpdatesOptions {
            job_id: "j1".to_owned(),
            channel: recorder.clone(),
            writer: Some(Arc::new(PayloadRefWriter::new(PayloadRefWriterOptions {
                root: root.path().to_path_buf(),
                generation: 1,
                budget: None,
                now: None,
            }))),
            window: None,
        });
        let big =
            stream.update(json!({ "big": "x".repeat(TOOL_CHILD_LIMITS.ref_threshold + 1024) }));
        let small = stream.update(json!({ "small": true }));
        // Awaited in reverse: the order is the call order, not the await order.
        let (small, big) = (small.await, big.await);
        assert_eq!((big, small), (Ok(()), Ok(())));
        assert_eq!(*lock(&recorder.frames), vec![(1, true), (2, false)]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn releases_a_producer_waiting_for_credit_when_its_job_ends_with_an_error_rather_than_a_hang()
     {
        let recorder = Arc::new(UpdateRecorder::default());
        let stream = JobUpdates::new(JobUpdatesOptions {
            job_id: "j1".to_owned(),
            channel: recorder.clone(),
            writer: None,
            window: Some(1),
        });
        assert_eq!(stream.update(json!("first")).await, Ok(()));
        let blocked = tokio::spawn(stream.update(json!("second")));
        // The producer is parked on credit once the second update has not gone out and the task was polled.
        tokio::task::yield_now().await;
        stream.end();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), blocked).await;
        let Ok(Ok(Err(error))) = outcome else {
            panic!("expected the blocked update to fail, got {outcome:?}");
        };
        assert_eq!(error.to_string(), "update after job j1 ended");
    }
}
