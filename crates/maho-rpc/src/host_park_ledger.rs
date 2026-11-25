//! Owns both in-flight builds and ready parks, including the races with claim, expiry and shutdown.

use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::watch;

/// Speculation is bounded independently of the unlimited in-process session registry.
pub const MAX_PARKS: usize = 4;
pub const PARK_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkResult {
    Parked,
    AlreadyParked,
    Full,
}

struct ParkEntry<S> {
    ready_tx: watch::Sender<Option<Result<(), String>>>,
    opened: Option<S>,
    expiry: Option<tokio::task::JoinHandle<()>>,
    claimed: bool,
    claim_tx: Option<watch::Sender<bool>>,
    closing: Option<watch::Receiver<bool>>,
}

struct LedgerState<S> {
    parks: HashMap<String, Arc<Mutex<ParkEntry<S>>>>,
    replacements: HashMap<String, tokio::task::JoinHandle<()>>,
    stopped: bool,
}

impl<S> Default for LedgerState<S> {
    fn default() -> Self {
        Self {
            parks: HashMap::new(),
            replacements: HashMap::new(),
            stopped: false,
        }
    }
}

pub struct HostParkLedger<S> {
    state: Arc<Mutex<LedgerState<S>>>,
    close: Arc<dyn Fn(S) -> BoxFuture<'static, ()> + Send + Sync>,
    ttl: Duration,
}

impl<S> Clone for HostParkLedger<S> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            close: self.close.clone(),
            ttl: self.ttl,
        }
    }
}

impl<S: Clone + Send + Sync + 'static> HostParkLedger<S> {
    pub fn new(close: impl Fn(S) -> BoxFuture<'static, ()> + Send + Sync + 'static) -> Self {
        Self::with_ttl(close, Duration::from_millis(PARK_TTL_MS))
    }

    pub fn with_ttl(
        close: impl Fn(S) -> BoxFuture<'static, ()> + Send + Sync + 'static,
        ttl: Duration,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(LedgerState::default())),
            close: Arc::new(close),
            ttl,
        }
    }

    pub fn size(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .parks
            .len()
    }

    pub async fn park(
        &self,
        key: &str,
        build: impl FnOnce() -> BoxFuture<'static, Result<S, String>>,
    ) -> Result<ParkResult, String> {
        enum NextStep<S> {
            WaitExisting(watch::Receiver<Option<Result<(), String>>>),
            Return(Result<ParkResult, String>),
            Build(Arc<Mutex<ParkEntry<S>>>),
        }
        let step = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = state.parks.get(key).cloned() {
                let (claimed, closing, rx) = {
                    let g = existing.lock().unwrap_or_else(PoisonError::into_inner);
                    (g.claimed, g.closing.is_some(), g.ready_tx.subscribe())
                };
                if !claimed && !closing {
                    NextStep::WaitExisting(rx)
                } else {
                    // A park being claimed or closed is on its way out; the replacement scheduled after it builds the next.
                    NextStep::Return(Ok(ParkResult::Full))
                }
            } else {
                if let Some(replacement) = state.replacements.remove(key) {
                    replacement.abort();
                }
                if state.stopped {
                    NextStep::Return(Err("host_draining".to_owned()))
                } else if state.parks.len() + state.replacements.len() >= MAX_PARKS {
                    NextStep::Return(Ok(ParkResult::Full))
                } else {
                    let (ready_tx, _) = watch::channel(None);
                    let entry = Arc::new(Mutex::new(ParkEntry {
                        ready_tx,
                        opened: None,
                        expiry: None,
                        claimed: false,
                        claim_tx: None,
                        closing: None,
                    }));
                    // Reserved before build's first await: a second request joins, never starts another runtime.
                    state.parks.insert(key.to_owned(), entry.clone());
                    NextStep::Build(entry)
                }
            }
        };
        let entry = match step {
            NextStep::Return(res) => return res,
            NextStep::WaitExisting(mut rx) => {
                let res = rx
                    .wait_for(Option::is_some)
                    .await
                    .map_err(|_| "park cancelled".to_owned())?;
                if let Some(Err(cause)) = res.as_ref() {
                    return Err(cause.clone());
                }
                return Ok(ParkResult::AlreadyParked);
            }
            NextStep::Build(entry) => entry,
        };

        let built = build().await;
        match built {
            Ok(opened) => {
                let mut stopped_now = false;
                {
                    let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    let mut g = entry.lock().unwrap_or_else(PoisonError::into_inner);
                    g.opened = Some(opened);
                    if state.stopped {
                        stopped_now = true;
                    } else if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        let ledger = self.clone();
                        let k = key.to_owned();
                        let e = entry.clone();
                        let ttl = self.ttl;
                        g.expiry = Some(handle.spawn(async move {
                            tokio::time::sleep(ttl).await;
                            ledger.retire(&k, &e).await;
                        }));
                    }
                    g.ready_tx.send_replace(Some(Ok(())));
                }
                if stopped_now {
                    self.retire(key, &entry).await;
                }
                Ok(ParkResult::Parked)
            }
            Err(cause) => {
                {
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state
                        .parks
                        .get(key)
                        .is_some_and(|cur| Arc::ptr_eq(cur, &entry))
                    {
                        state.parks.remove(key);
                    }
                }
                let g = entry.lock().unwrap_or_else(PoisonError::into_inner);
                g.ready_tx.send_replace(Some(Err(cause.clone())));
                Err(cause)
            }
        }
    }

    /// A claim never waits for a build, and consumes the ready park synchronously.
    pub fn take(&self, key: &str) -> Option<S> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped {
            return None;
        }
        let park = state.parks.get(key)?;
        let mut g = park.lock().unwrap_or_else(PoisonError::into_inner);
        if g.opened.is_none() || g.claimed || g.closing.is_some() {
            return None;
        }
        if let Some(expiry) = g.expiry.take() {
            expiry.abort();
        }
        g.claimed = true;
        let (claim_tx, _) = watch::channel(false);
        g.claim_tx = Some(claim_tx);
        g.opened.clone()
    }

    pub fn finish(&self, key: &str) {
        let park = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .parks
            .remove(key);
        if let Some(park) = park {
            let g = park.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(tx) = &g.claim_tx {
                tx.send_replace(true);
            }
        }
    }

    pub fn replace(
        &self,
        key: String,
        build: impl FnOnce() -> BoxFuture<'static, Result<S, String>> + Send + 'static,
    ) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped || state.replacements.contains_key(&key) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let ledger = self.clone();
        let k = key.clone();
        let task = handle.spawn(async move {
            tokio::task::yield_now().await;
            {
                let mut state = ledger.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.replacements.remove(&k);
                if state.stopped {
                    return;
                }
            }
            if let Err(cause) = ledger.park(&k, build).await {
                eprintln!("senpi rpc replacement park failed: {cause}");
            }
        });
        state.replacements.insert(key, task);
    }

    /// Fences new builds synchronously; every build already accepted is closed when it finishes.
    pub async fn stop(&self) {
        let snapshot: Vec<(String, Arc<Mutex<ParkEntry<S>>>)> = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.stopped = true;
            for (_, task) in state.replacements.drain() {
                task.abort();
            }
            state
                .parks
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        for (key, park) in snapshot {
            let mut ready_rx = park
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ready_tx
                .subscribe();
            let _ = ready_rx.wait_for(Option::is_some).await;
            let claim_rx = {
                let g = park.lock().unwrap_or_else(PoisonError::into_inner);
                if g.claimed {
                    g.claim_tx.as_ref().map(watch::Sender::subscribe)
                } else {
                    None
                }
            };
            if let Some(mut rx) = claim_rx {
                let _ = rx.wait_for(|done| *done).await;
            } else {
                self.retire(&key, &park).await;
            }
        }
    }

    async fn retire(&self, key: &str, park: &Arc<Mutex<ParkEntry<S>>>) {
        let (existing_rx, opened, tx_done) = {
            let mut g = park.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(expiry) = g.expiry.take() {
                expiry.abort();
            }
            if let Some(rx) = &g.closing {
                (Some(rx.clone()), None, None)
            } else {
                let (tx, rx) = watch::channel(false);
                g.closing = Some(rx);
                (None, g.opened.take(), Some(tx))
            }
        };
        if let Some(mut rx) = existing_rx {
            let _ = rx.wait_for(|done| *done).await;
            return;
        }
        if let Some(opened) = opened {
            (self.close)(opened).await;
        }
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state
                .parks
                .get(key)
                .is_some_and(|cur| Arc::ptr_eq(cur, park))
            {
                state.parks.remove(key);
            }
        }
        if let Some(tx) = tx_done {
            tx.send_replace(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn parks_coalesces_bounds_capacity_claims_and_stops() {
        let closed = Arc::new(Mutex::new(Vec::<String>::new()));
        let closed_sink = closed.clone();
        let ledger = HostParkLedger::new(move |s: String| {
            let sink = closed_sink.clone();
            async move {
                sink.lock().unwrap_or_else(PoisonError::into_inner).push(s);
            }
            .boxed()
        });

        let builds = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = watch::channel(false);
        let b1 = builds.clone();
        let mut r1 = gate_rx.clone();
        let fut1 = ledger.park("k1", || {
            b1.fetch_add(1, Ordering::SeqCst);
            async move {
                let _ = r1.wait_for(|open| *open).await;
                Ok("sess-1".to_owned())
            }
            .boxed()
        });
        let b2 = builds.clone();
        let fut2 = ledger.park("k1", || {
            b2.fetch_add(1, Ordering::SeqCst);
            async { Ok("sess-dup".to_owned()) }.boxed()
        });
        let (res1, res2, _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(fut1, fut2, async {
                gate_tx.send_replace(true);
            })
        })
        .await
        .expect("join park k1");
        assert_eq!(res1, Ok(ParkResult::Parked));
        assert_eq!(res2, Ok(ParkResult::AlreadyParked));
        assert_eq!(builds.load(Ordering::SeqCst), 1);

        assert_eq!(ledger.take("k1"), Some("sess-1".to_owned()));
        assert_eq!(ledger.take("k1"), None);
        ledger.finish("k1");
        assert_eq!(ledger.size(), 0);

        for i in 0..MAX_PARKS {
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    ledger.park(&format!("p{i}"), move || async move { Ok(format!("s{i}")) }
                        .boxed()),
                )
                .await
                .expect("park loop"),
                Ok(ParkResult::Parked)
            );
        }
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                ledger.park("overflow", || async { Ok("x".to_owned()) }.boxed()),
            )
            .await
            .expect("park overflow"),
            Ok(ParkResult::Full)
        );

        tokio::time::timeout(Duration::from_secs(2), ledger.stop())
            .await
            .expect("stop");
        assert_eq!(ledger.size(), 0);
        assert_eq!(
            closed.lock().unwrap_or_else(PoisonError::into_inner).len(),
            MAX_PARKS
        );
        assert_eq!(
            ledger
                .park("after-stop", || async { Ok("x".to_owned()) }.boxed())
                .await,
            Err("host_draining".to_owned())
        );
    }
}
