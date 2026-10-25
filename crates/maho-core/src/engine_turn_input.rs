//! Port of senpi packages/coding-agent/src/core/engine-turn-input.ts.
//!
//! Input owns one release even when a command dispatches another prompt or a human steers mid-turn.

use std::collections::BTreeMap;
use std::future::Future;

use serde_json::Value;

use crate::engine_turn_hold::{EngineTurnHold, HoldReleaseCause};
use crate::engine_turn_hold_notices::HeldMessage;

struct Pending {
    sources: BTreeMap<String, usize>,
    wakes: Vec<HeldMessage>,
    admitted: bool,
    consume_at_settlement: bool,
}

#[derive(Default)]
pub struct EngineTurnInput {
    pending: Option<Pending>,
}

impl EngineTurnInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn begin(&mut self, hold: &mut EngineTurnHold) {
        self.begin_with_options(hold, false);
    }

    pub fn begin_with_options(&mut self, hold: &mut EngineTurnHold, consume_at_settlement: bool) {
        hold.attend();
        if hold.releasing() || self.pending.is_some() || !hold.held() {
            return;
        }
        let wakes = hold.messages();
        let Ok(sources) = hold.release_all(HoldReleaseCause::Input) else {
            return;
        };
        self.pending = Some(Pending {
            sources,
            wakes,
            admitted: false,
            consume_at_settlement,
        });
    }

    pub fn messages(&self) -> Vec<Value> {
        match &self.pending {
            Some(pending) if !pending.admitted => pending
                .wakes
                .iter()
                .map(|wake| wake.message.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn admit(
        &mut self,
        hold: &mut EngineTurnHold,
        queued: bool,
        mut enqueue: impl FnMut(Value),
    ) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if pending.admitted {
            return;
        }
        if pending.consume_at_settlement {
            if queued { for wake in &pending.wakes { enqueue(wake.message.clone()); } }
            pending.admitted = true;
            return;
        }
        for wake in &pending.wakes {
            if hold.consume(&wake.source, Some(wake.notice_ids.as_slice())) && queued {
                enqueue(wake.message.clone());
            }
        }
        pending.admitted = true;
    }

    /// The synchronous half of `settle`: an admitted release finishes and yields the sources to announce;
    /// one no input admitted is given back with custody intact (`None`). Callers that cannot hold the hold
    /// across an await call this, then announce the sources themselves.
    pub fn take_settlement(
        &mut self,
        hold: &mut EngineTurnHold,
    ) -> Option<BTreeMap<String, usize>> {
        self.take_settlement_with_options(hold, false)
    }

    pub fn reject(&mut self, hold: &mut EngineTurnHold) {
        if self.pending.take().is_some() { hold.reject_input(); }
    }

    pub fn take_settlement_with_options(&mut self, hold: &mut EngineTurnHold, newer_stop: bool) -> Option<BTreeMap<String, usize>> {
        let pending = self.pending.take()?;
        if !pending.admitted {
            hold.abandon_release();
            return None;
        }
        if pending.consume_at_settlement {
            if newer_stop { hold.reject_input(); return None; }
            for wake in &pending.wakes { hold.consume(&wake.source, Some(&wake.notice_ids)); }
        }
        let mut merged = pending.sources;
        merged.extend(hold.finish());
        Some(merged)
    }

    /// The one settle point, called once the session is quiescent: an admitted release finishes; one no
    /// input admitted is given back with custody intact.
    pub async fn settle<R, Fut>(
        &mut self,
        hold: &mut EngineTurnHold,
        release: R,
    ) -> Result<(), String>
    where
        R: FnOnce(BTreeMap<String, usize>) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        match self.take_settlement(hold) {
            Some(merged) => release(merged).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_turn_hold::{HoldAppend, HoldEmit, HoldNotify};
    use serde_json::json;
    use std::sync::Arc;

    fn hold() -> EngineTurnHold {
        let append: HoldAppend = Arc::new(|_, _| Ok(()));
        let notify: HoldNotify = Arc::new(|_| {});
        let emit: HoldEmit = Arc::new(|_| {});
        let mut hold = EngineTurnHold::new(&[], true, append, notify, emit);
        hold.start();
        hold.note("monitor", Some(json!({"content": "ev"})));
        hold
    }

    #[test]
    fn settlement_keeps_notices_when_a_newer_stop_intervenes() {
        let mut hold = hold();
        let mut input = EngineTurnInput::new();
        input.begin_with_options(&mut hold, true);
        input.admit(&mut hold, true, |_| {});
        assert_eq!(hold.notice_count(), 1);
        hold.add_hold(crate::engine_turn_hold::HoldKind::SessionHold, None, None);
        assert!(input.take_settlement_with_options(&mut hold, true).is_none());
        assert_eq!(hold.notice_count(), 1);
        assert!(hold.held());
        input.begin_with_options(&mut hold, true);
        input.admit(&mut hold, false, |_| {});
        assert!(input.take_settlement_with_options(&mut hold, false).is_some());
        assert_eq!(hold.notice_count(), 0);
    }

    #[tokio::test]
    async fn unadmitted_settle_abandons_and_admitted_settle_releases() {
        let mut hold = hold();
        let mut input = EngineTurnInput::new();
        input.begin(&mut hold);
        assert!(input.has_pending() && !hold.held() && input.messages().len() == 1);
        assert!(input.settle(&mut hold, |_| async { Ok(()) }).await.is_ok());
        assert!(hold.held() && !input.has_pending());

        input.begin(&mut hold);
        let mut queued = Vec::new();
        input.admit(&mut hold, true, |message| queued.push(message));
        assert_eq!(queued.len(), 1);
        assert!(input.messages().is_empty());
        let seen = std::sync::Mutex::new(None);
        let result = input
            .settle(&mut hold, |sources| {
                if let Ok(mut seen) = seen.lock() {
                    *seen = Some(sources);
                }
                async { Ok(()) }
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(
            seen.lock()
                .ok()
                .map(|s| s.as_ref().and_then(|m| m.get("monitor").copied())),
            Some(Some(1))
        );
        assert!(!hold.releasing());
    }
}
