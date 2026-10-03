use std::{future::Future, time::Duration};

pub const INTERRUPT_ACK_MS: u64 = 500;
pub const JS_INTERRUPT_GRACE_MS: u64 = 2000;
pub const WORKER_TERMINATE_DEADLINE_MS: u64 = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CooperativeSettlement { Settled, Unresponsive }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerRetirement { Terminated, Abandoned }

pub async fn await_cooperative_settlement<S, A>(settled: bool, settlement: S, acknowledgement: Option<A>, ack_ms: u64, grace_ms: u64) -> CooperativeSettlement
where S: Future<Output = ()>, A: Future<Output = ()> {
    if settled { return CooperativeSettlement::Settled; }
    tokio::pin!(settlement);
    let first = async {
        match acknowledgement {
            Some(acknowledgement) => tokio::select! {
                () = &mut settlement => true,
                () = acknowledgement => false,
            },
            None => false,
        }
    };
    match tokio::time::timeout(Duration::from_millis(ack_ms), first).await {
        Ok(true) => CooperativeSettlement::Settled,
        Ok(false) => match tokio::time::timeout(Duration::from_millis(grace_ms), settlement).await {
            Ok(()) => CooperativeSettlement::Settled,
            Err(_) => CooperativeSettlement::Unresponsive,
        },
        Err(_) => CooperativeSettlement::Unresponsive,
    }
}

pub async fn retire_worker<F>(termination: F, deadline_ms: u64) -> WorkerRetirement
where F: Future<Output = ()> {
    match tokio::time::timeout(Duration::from_millis(deadline_ms), termination).await {
        Ok(()) => WorkerRetirement::Terminated,
        Err(_) => WorkerRetirement::Abandoned,
    }
}

pub fn abandoned_worker_note(deadline_ms: u64) -> String {
    format!("JavaScript worker did not stop within {deadline_ms}ms: a synchronous call (for example Bun.spawnSync or child_process.spawnSync) is blocking it. A fresh worker replaced it; the blocked call keeps running until it returns.\n")
}
