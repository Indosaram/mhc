//! Port of senpi extensions/extension-handle-ledger.ts (senpi#2292).
//!
//! Servers, sockets and timers an extension leaves open after its `session_shutdown` handlers are
//! closed by the host and reported once per extension. JS net/timer monkey-patching has no native
//! equivalent, so ownership is by scope: code running inside [`run_owned_by`] / [`owned_by`] records
//! handles it registers through [`track_handle`], and the ledger of that extension closes them.
use maho_ext_api::{
    EventResult, ExtensionContext, ExtensionEvent, ExtensionFuture, ExtensionHandler,
    LoadedExtension,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnedHandleKind {
    Server,
    Socket,
    Timer,
}
impl OwnedHandleKind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Socket => "socket",
            Self::Timer => "timer",
        }
    }
}

/// Closes one handle; the error text names why it could not be closed.
pub type HandleRelease = Box<dyn FnOnce() -> Result<(), String> + Send>;

#[derive(Default)]
struct LedgerState {
    next_id: u64,
    open: BTreeMap<u64, (OwnedHandleKind, HandleRelease)>,
    closed: bool,
}

/// What `reclaim` closed, per kind, and the handles that refused to close.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    pub servers: usize,
    pub sockets: usize,
    pub timers: usize,
    pub failures: Vec<String>,
}
impl Reclaimed {
    pub const fn total(&self) -> usize {
        self.servers + self.sockets + self.timers
    }
}

/// Per extension instance: the handles its code created and has not released yet. A closed ledger
/// records nothing new.
#[derive(Default)]
pub struct ExtensionHandleLedger {
    state: Mutex<LedgerState>,
}

/// A tracked handle; call [`TrackedHandle::untrack`] once the extension closed it itself.
pub struct TrackedHandle {
    ledger: Arc<ExtensionHandleLedger>,
    id: u64,
}
impl TrackedHandle {
    pub fn untrack(self) {
        self.ledger
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open
            .remove(&self.id);
    }
}

impl ExtensionHandleLedger {
    /// Records an open handle; `None` once the ledger has been reclaimed.
    pub fn track(
        self: &Arc<Self>,
        kind: OwnedHandleKind,
        release: HandleRelease,
    ) -> Option<TrackedHandle> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return None;
        }
        let id = state.next_id;
        state.next_id += 1;
        state.open.insert(id, (kind, release));
        Some(TrackedHandle {
            ledger: Arc::clone(self),
            id,
        })
    }
    /// Runs once, after the instance's `session_shutdown` handlers: closes whatever is still open.
    pub fn reclaim(&self) -> Reclaimed {
        let open = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            std::mem::take(&mut state.open)
        };
        let mut out = Reclaimed::default();
        for (_, (kind, release)) in open {
            match release() {
                Ok(()) => match kind {
                    OwnedHandleKind::Server => out.servers += 1,
                    OwnedHandleKind::Socket => out.sockets += 1,
                    OwnedHandleKind::Timer => out.timers += 1,
                },
                Err(message) => out.failures.push(format!("{}: {message}", kind.noun())),
            }
        }
        out
    }
}

tokio::task_local! { static CURRENT_OWNER: Arc<ExtensionHandleLedger>; }

/// Records a handle against the extension whose code is running; `None` when untracked.
pub fn track_handle(kind: OwnedHandleKind, release: HandleRelease) -> Option<TrackedHandle> {
    CURRENT_OWNER
        .try_with(|ledger| ledger.track(kind, release))
        .ok()
        .flatten()
}

/// Builtin extensions are the host's own code and inline factories (`<...>` paths) have no code
/// location of their own, so neither is tracked.
pub fn is_untracked(extension: &LoadedExtension) -> bool {
    extension.source_info.source == "builtin" || extension.identity.path.starts_with('<')
}

/// Runs `task` as the owner's code: handles it registers are recorded for that ledger.
pub async fn run_owned_by<F: std::future::Future>(
    ledger: Option<Arc<ExtensionHandleLedger>>,
    task: F,
) -> F::Output {
    match ledger {
        Some(ledger) => CURRENT_OWNER.scope(ledger, task).await,
        None => task.await,
    }
}

/// `handler`, entered as the owner's code on every call; `None` ledgers get the handler back.
pub fn owned_by(
    ledger: Option<Arc<ExtensionHandleLedger>>,
    handler: ExtensionHandler,
) -> ExtensionHandler {
    let Some(ledger) = ledger else { return handler };
    Arc::new(
        move |event: &mut ExtensionEvent, context: &ExtensionContext| {
            let ledger = Arc::clone(&ledger);
            let handler = Arc::clone(&handler);
            let future: ExtensionFuture<'_, EventResult> =
                Box::pin(CURRENT_OWNER.scope(ledger, handler(event, context)));
            future
        },
    )
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// The one-per-extension warning, or `None` when nothing was left open.
pub fn leak_warning(path: &str, reclaimed: &Reclaimed) -> Option<String> {
    if reclaimed.total() == 0 && reclaimed.failures.is_empty() {
        return None;
    }
    let closed = [
        plural(reclaimed.servers, "server"),
        plural(reclaimed.sockets, "socket"),
        plural(reclaimed.timers, "timer"),
    ]
    .join(", ");
    let failed = if reclaimed.failures.is_empty() {
        String::new()
    } else {
        format!("; could not close {}", reclaimed.failures.join("; "))
    };
    Some(format!(
        "Extension {path} left handles open after session_shutdown; maho closed {closed}{failed}. Stop session-scoped resources in a session_shutdown handler (docs/extensions.md#session_shutdown)."
    ))
}

/// The ledgers of one runner's tracked extensions, in load order.
#[derive(Clone, Default)]
pub struct RunnerHandleLedgers {
    by_path: Arc<BTreeMap<String, Arc<ExtensionHandleLedger>>>,
}
impl RunnerHandleLedgers {
    /// `registerRunnerExtensions`: one ledger per tracked extension.
    pub fn register_runner_extensions(extensions: &[LoadedExtension]) -> Self {
        let by_path = extensions
            .iter()
            .filter(|extension| !is_untracked(extension))
            .map(|extension| {
                (
                    extension.identity.path.clone(),
                    Arc::new(ExtensionHandleLedger::default()),
                )
            })
            .collect();
        Self {
            by_path: Arc::new(by_path),
        }
    }
    pub fn ledger_for(&self, path: &str) -> Option<Arc<ExtensionHandleLedger>> {
        self.by_path.get(path).cloned()
    }
    /// `reclaimRunnerHandles`: closes what each extension left open and returns one warning per extension.
    pub fn reclaim_all(&self) -> Vec<String> {
        self.by_path
            .iter()
            .filter_map(|(path, ledger)| leak_warning(path, &ledger.reclaim()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn reclaim_closes_open_handles_once_and_untracked_ones_stay_out() {
        let ledger = Arc::new(ExtensionHandleLedger::default());
        let closed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&closed);
        assert!(
            ledger
                .track(
                    OwnedHandleKind::Server,
                    Box::new(move || {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                )
                .is_some()
        );
        let released = ledger.track(OwnedHandleKind::Timer, Box::new(|| Ok(())));
        if let Some(handle) = released {
            handle.untrack();
        }
        assert!(
            ledger
                .track(OwnedHandleKind::Socket, Box::new(|| Err("busy".into())))
                .is_some()
        );
        let reclaimed = ledger.reclaim();
        assert_eq!(
            (reclaimed.servers, reclaimed.sockets, reclaimed.timers),
            (1, 0, 0)
        );
        assert_eq!(reclaimed.failures, vec!["socket: busy".to_owned()]);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(
            ledger
                .track(OwnedHandleKind::Timer, Box::new(|| Ok(())))
                .is_none()
        );
        assert_eq!(ledger.reclaim(), Reclaimed::default());
    }

    #[test]
    fn warning_names_extension_and_pluralizes() {
        let reclaimed = Reclaimed {
            servers: 1,
            sockets: 2,
            timers: 0,
            failures: vec!["timer: x".into()],
        };
        assert_eq!(
            leak_warning("/ext/a.ts", &reclaimed).as_deref(),
            Some(
                "Extension /ext/a.ts left handles open after session_shutdown; maho closed 1 server, 2 sockets, 0 timers; could not close timer: x. Stop session-scoped resources in a session_shutdown handler (docs/extensions.md#session_shutdown)."
            )
        );
        assert_eq!(leak_warning("/ext/a.ts", &Reclaimed::default()), None);
    }

    #[tokio::test]
    async fn tracking_is_scoped_to_the_owning_extension() {
        let ledger = Arc::new(ExtensionHandleLedger::default());
        assert!(track_handle(OwnedHandleKind::Timer, Box::new(|| Ok(()))).is_none());
        let tracked = run_owned_by(Some(Arc::clone(&ledger)), async {
            track_handle(OwnedHandleKind::Timer, Box::new(|| Ok(()))).is_some()
        })
        .await;
        assert!(tracked);
        assert_eq!(ledger.reclaim().timers, 1);
    }

    #[test]
    fn builtin_and_inline_extensions_are_untracked() {
        let mut builtin = LoadedExtension::new(
            "/x/b.ts",
            "/".into(),
            maho_ext_api::SourceInfo {
                source: "builtin".into(),
                ..Default::default()
            },
        );
        assert!(is_untracked(&builtin));
        builtin.source_info.source = "local".into();
        assert!(!is_untracked(&builtin));
        assert!(is_untracked(&LoadedExtension::new(
            "<inline:1>",
            "/".into(),
            maho_ext_api::SourceInfo::default()
        )));
    }
}
