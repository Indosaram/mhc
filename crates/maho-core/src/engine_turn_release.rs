//! Port of senpi packages/coding-agent/src/core/engine-turn-release.ts.
//!
//! Never uses the native follow-up queue: its `all` mode may combine unrelated sources.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::engine_turn_hold_notices::HeldMessage;

/// The slice of EngineTurnHold the release flow drives.
pub trait ReleasableHold {
    fn held(&self) -> bool;
    fn releasing(&self) -> bool;
    fn generation(&self) -> u64;
    fn release_all_resume_session(&mut self) -> BTreeMap<String, usize>;
    fn messages(&self) -> Vec<HeldMessage>;
    fn consume(&mut self, source: &str, notice_ids: &[String]) -> bool;
    fn reject_input(&mut self);
    fn finish(&mut self) -> BTreeMap<String, usize>;
}

#[derive(Debug, thiserror::Error)]
pub enum EngineTurnReleaseError {
    #[error("{0}")]
    Delivery(String),
}

#[derive(Default)]
pub struct EngineTurnRelease {
    running: AtomicBool,
}

impl EngineTurnRelease {
    pub fn new() -> Self {
        Self::default()
    }

    /// Single-flight: a concurrent call while a release runs is a no-op.
    pub async fn resume<H, S, SFut, R, RFut>(
        &self,
        hold: &mut H,
        mut send_through_agent_end: S,
        release_sources: R,
    ) -> Result<(), EngineTurnReleaseError>
    where
        H: ReleasableHold,
        S: FnMut(Value, String, Vec<String>) -> SFut,
        SFut: Future<Output = Result<bool, String>>,
        R: FnOnce(BTreeMap<String, usize>) -> RFut,
        RFut: Future<Output = Result<(), String>>,
    {
        if self.running.load(Ordering::SeqCst) || !hold.held() || hold.releasing() {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);
        let result = self
            .run(hold, &mut send_through_agent_end, release_sources)
            .await;
        self.running.store(false, Ordering::SeqCst);
        result
    }

    async fn run<H, S, SFut, R, RFut>(
        &self,
        hold: &mut H,
        send: &mut S,
        release_sources: R,
    ) -> Result<(), EngineTurnReleaseError>
    where
        H: ReleasableHold,
        S: FnMut(Value, String, Vec<String>) -> SFut,
        SFut: Future<Output = Result<bool, String>>,
        R: FnOnce(BTreeMap<String, usize>) -> RFut,
        RFut: Future<Output = Result<(), String>>,
    {
        let sources = hold.release_all_resume_session();
        let generation = hold.generation();
        for held in hold.messages() {
            if hold.generation() != generation {
                return Ok(());
            }
            // The callee admits through hold.consume(source, noticeIds) once the run accepts the message.
            let delivered = match send(
                held.message.clone(),
                held.source.clone(),
                held.notice_ids.clone(),
            )
            .await
            {
                Ok(delivered) => delivered,
                Err(error) => {
                    if hold.generation() == generation {
                        hold.reject_input();
                    }
                    return Err(EngineTurnReleaseError::Delivery(error));
                }
            };
            if hold.generation() != generation {
                return Ok(());
            }
            if !delivered {
                hold.reject_input();
                return Ok(());
            }
            hold.consume(&held.source, &held.notice_ids);
        }
        if hold.generation() != generation {
            return Ok(());
        }
        let mut merged = sources;
        merged.extend(hold.finish());
        if !hold.held()
            && let Err(error) = release_sources(merged).await
        {
            hold.reject_input();
            return Err(EngineTurnReleaseError::Delivery(error));
        }
        Ok(())
    }
}
