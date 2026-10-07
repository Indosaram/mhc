//! Shared live per-session resources for the resident Kibitzer.
//!
//! Upstream keeps the state one sidecar shares with its child's tool closures on two records: the
//! `SidecarCore` state record (`kibitzer/sidecar-core.ts`: `offered`, `surfaced`, `accepted`,
//! `budget`) and the tool object (`kibitzer/tools/index.ts`: the lifetime `searchedPaths` set plus the
//! `nudge` / `budget` binding handed to `createKibitzerSidecarTools`). This module is that state as ONE
//! clonable per-session handle, so the CLI child factory (which builds the five member tools) and the
//! resident sidecar (which resets the wake budget and re-validates the accepted nudges) resolve the
//! SAME slots for a session instead of each keeping a private copy.
//!
//! Cloning a [`KibitzerSessionResources`] shares the slots and live sets; it never copies the current
//! accepted list or the current wake's budget. There is no process-global registry: one
//! [`KibitzerSessionResourceRegistry`] is built per recall wiring, and its getter is the single
//! accessor forwarded unchanged to both consumers.
//!
//! # Binding the two member executors
//!
//! The member-tool unit owns registration; these are the exact slots it reads, and every borrow below
//! is taken at call time so a live change (a new offered/searched path, a swapped budget or accepted
//! list) is visible without rebuilding a tool:
//!
//! * `memory` - lock [`KibitzerSessionResources::corpus_cache`] for `&mut RecallCorpusCache` and
//!   [`KibitzerSessionResources::searched`] for `&mut BTreeSet<String>`, then call
//!   `kibitzer_tools_memory::execute_memory(repo, cache, caps, query_expansion, searched, params)`.
//! * `nudge` - the allowed set is `offered ∪ searched` plus the session's [`KibitzerSessionResources::max_items`];
//!   lock [`KibitzerSessionResources::surfaced`] for `&BTreeSet<String>` and take the current list from
//!   [`KibitzerAcceptedSlot::current`] for `&mut Vec<RecallNudge>`, then call
//!   `kibitzer_nudge_tool::execute_nudge(input, params)`.
//! * every tool - charge [`KibitzerSessionResources::budget_slot`]'s `current()` ONCE on entry.
//!
//! # Lifecycle
//!
//! * Admission of a NEW turn swaps the accepted list and the budget: the sidecar calls
//!   [`KibitzerAcceptedSlot::reset`] and [`KibitzerBudgetSlot::reset`]. A retained closure reads the
//!   replacement through `current()`; a turn that is already running keeps the handles it took.
//! * A steer joins the running turn and MUST NOT reset either slot.
//! * Session shutdown disposes the child FIRST, then calls [`KibitzerSessionResourceRegistry::remove`].
//!   Removal is TERMINAL for that session: the registry forgets its ownership and a later lookup never
//!   recreates live state, so a late offer cannot bind a working child for a disposed session
//!   (upstream `kibitzer/sidecar.ts` ends a bound session's lifecycle at `shutdown`). Removal does not
//!   dispose a child; it is not lifecycle orchestration.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use memory_core::recall::{RecallCorpusCache, RecallCorpusCacheOptions, RecallNudge};

use crate::kibitzer_contract::{KIBITZER_WAKE_TOOL_BUDGET, KibitzerBudgetSlot, KibitzerToolBudget};
use crate::kibitzer_tools_budget::WakeToolBudget;

/// The CURRENT wake's accepted-nudge list, as a swappable slot.
///
/// Upstream keeps `core.accepted` as a plain field that `newTurn` REPLACES with a fresh array
/// (`sidecar-core.ts` / `sidecar-turn.ts`); the nudge closure reads `core.accepted` at call time, so it
/// always sees the current wake's list, and a steer - which joins the running turn - keeps it. This
/// slot gives a retained native closure the same view across a sidecar's lifetime: `current` hands out
/// the wake's list and `reset` swaps in a fresh one at admission, while a handle taken before the swap
/// keeps recording into the list it was taken from.
pub struct KibitzerAcceptedSlot {
    current: Mutex<Arc<Mutex<Vec<RecallNudge>>>>,
}

impl KibitzerAcceptedSlot {
    /// An empty slot for one bound session.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self { current: Mutex::new(Arc::new(Mutex::new(Vec::new()))) })
    }

    /// The stable current-slot getter: the list the CURRENT wake records into. A retained tool closure
    /// calls this per call, so it follows a later `reset`; a steer keeps the same list.
    pub fn current(&self) -> Arc<Mutex<Vec<RecallNudge>>> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Admission of a NEW turn: replace the current wake's list with a fresh empty one. A handle taken
    /// before the swap still owns the list it was taken from, so an active turn's snapshot is distinct.
    pub fn reset(&self) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(Mutex::new(Vec::new()));
    }
}

/// The CURRENT wake's tool-call budget: the session-owned [`KibitzerBudgetSlot`].
///
/// Every one of the five member tools charges this slot through `current()`, and the sidecar resets it
/// on admission, so all closures of a wake share ONE budget and each new wake gets a fresh one. The
/// slot - not a bare `WakeToolBudget` - is what the tools and the sidecar keep, so replacing the budget
/// never invalidates a retained handle.
///
/// A TERMINAL slot belongs to a removed session's inert handle. Its `reset` is a no-op, so the zero
/// budget it was built with can never be replaced by a live one: `reset(limit)` cannot re-enable
/// charging. Ordinary (non-terminal) slots keep the usual replace semantics.
struct SessionBudgetSlot {
    current: Mutex<Arc<dyn KibitzerToolBudget>>,
    /// True only for a removed session's inert handle; makes `reset` unable to re-enable charging.
    terminal: bool,
}

impl SessionBudgetSlot {
    fn new(limit: usize, terminal: bool) -> Arc<Self> {
        let budget: Arc<dyn KibitzerToolBudget> = WakeToolBudget::new(limit);
        Arc::new(Self { current: Mutex::new(budget), terminal })
    }
}

impl KibitzerBudgetSlot for SessionBudgetSlot {
    /// The stable current-slot getter: the budget every closure of the CURRENT wake charges.
    fn current(&self) -> Arc<dyn KibitzerToolBudget> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn reset(&self, limit: usize) {
        // A terminal (detached) slot keeps its zero budget: reset can never re-enable charging, even if
        // a late caller passes a live limit. Ordinary slots replace the budget as usual.
        if self.terminal {
            return;
        }
        let budget: Arc<dyn KibitzerToolBudget> = WakeToolBudget::new(limit);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = budget;
    }
}

/// The live per-session state shared by one session's sidecar and every tool closure of its child.
///
/// Cloning shares the slots and live sets; it never copies the current accepted list or the current
/// wake's budget. The child factory and the runner must resolve this through the ONE registry getter,
/// never build a second one.
#[derive(Clone)]
pub struct KibitzerSessionResources {
    /// The CURRENT wake's tool-call budget, shared by all five member tools and reset by the sidecar
    /// on admission.
    pub budget_slot: Arc<dyn KibitzerBudgetSlot>,
    /// Paths offered to this sidecar lifetime; grows with every wake.
    pub offered: Arc<Mutex<BTreeSet<String>>>,
    /// Paths already surfaced in this session; they never repeat.
    pub surfaced: Arc<Mutex<BTreeSet<String>>>,
    /// Corpus-verified paths the `memory` tool returned through `search`: nudge-eligible for the
    /// sidecar's lifetime and the second half of the nudge tool's allowed set.
    pub searched: Arc<Mutex<BTreeSet<String>>>,
    /// The CURRENT wake's accepted-nudge list, swapped on admission.
    pub accepted: Arc<KibitzerAcceptedSlot>,
    /// The member-owned recall corpus cache: one per session, so a `memory` search and read of the same
    /// session share the loaded corpus and a HEAD move refreshes it once.
    pub corpus_cache: Arc<Mutex<RecallCorpusCache>>,
    /// `memory.recall.max_items` bound to this child: the child factory sets it from the spawn input
    /// before the tools are built, and the nudge tool reads it per call. It is `0` until then.
    max_items: Arc<AtomicUsize>,
    /// True only for the inert handle a REMOVED session resolves to; a live session is never detached.
    detached: bool,
}

impl KibitzerSessionResources {
    /// Fresh state for one bound session: a budget at the pinned wake default, empty live sets, a fresh
    /// accepted slot, an empty corpus cache and `max_items` at `0`. The registry calls this once per
    /// session; it is deliberately not `pub`, so a consumer cannot build a parallel one.
    pub(crate) fn new() -> Self {
        Self::with_budget(KIBITZER_WAKE_TOOL_BUDGET, false)
    }

    /// The inert handle a REMOVED session resolves to: a permanently exhausted budget slot, so every
    /// tool call is refused before it can act, and detached live sets that no other handle shares. It is
    /// never stored in the registry, so it can never become a session's live state.
    fn detached() -> Self {
        Self::with_budget(0, true)
    }

    fn with_budget(limit: usize, detached: bool) -> Self {
        Self {
            budget_slot: SessionBudgetSlot::new(limit, detached),
            offered: Arc::new(Mutex::new(BTreeSet::new())),
            surfaced: Arc::new(Mutex::new(BTreeSet::new())),
            searched: Arc::new(Mutex::new(BTreeSet::new())),
            accepted: KibitzerAcceptedSlot::new(),
            corpus_cache: Arc::new(Mutex::new(RecallCorpusCache::new(RecallCorpusCacheOptions::default()))),
            max_items: Arc::new(AtomicUsize::new(0)),
            detached,
        }
    }

    /// The session's current `memory.recall.max_items`.
    pub fn max_items(&self) -> usize {
        self.max_items.load(Ordering::SeqCst)
    }

    /// Bind the child's `memory.recall.max_items`, as the child factory does at spawn.
    pub fn bind_max_items(&self, max_items: usize) {
        self.max_items.store(max_items, Ordering::SeqCst);
    }

    /// True only for the inert handle of a removed session; a live session's handle is never detached.
    pub fn is_detached(&self) -> bool {
        self.detached
    }
}

#[derive(Default)]
struct RegistryState {
    sessions: BTreeMap<String, KibitzerSessionResources>,
    /// Sessions whose ownership was removed; they are terminal and never recreated.
    removed: BTreeSet<String>,
}

/// The per-session registry: the ONE place a bound session's [`KibitzerSessionResources`] live, shared
/// by the CLI child factory and the resident sidecar through a single getter.
///
/// There is deliberately no process-global instance: one registry is built per recall wiring, so two
/// mounts never see each other's sessions. The getter is created once and forwarded unchanged, so the
/// factory (which builds a child's five tools) and the runner (which resets the wake budget and
/// re-validates the accepted nudges) resolve the same handles for a session.
pub struct KibitzerSessionResourceRegistry {
    state: Mutex<RegistryState>,
}

impl KibitzerSessionResourceRegistry {
    /// An empty registry for one recall wiring.
    pub fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(RegistryState::default()) })
    }

    /// The ONE getter, shared by the child factory and the runner: two calls for one live session
    /// return handles to the same slots and live sets, and a session not yet seen gets fresh state. It
    /// never returns a global, never copies the current accepted list or budget, and NEVER recreates a
    /// removed session.
    pub fn getter(self: &Arc<Self>) -> Arc<dyn Fn(&str) -> KibitzerSessionResources + Send + Sync> {
        let registry = Arc::clone(self);
        Arc::new(move |session_id: &str| registry.for_session(session_id))
    }

    /// The resources for `session_id`, created on first use. The map is held only for this lookup. A
    /// REMOVED session is terminal: it resolves to an inert detached handle and is never recreated.
    pub fn for_session(&self, session_id: &str) -> KibitzerSessionResources {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(resources) = state.sessions.get(session_id) {
            return resources.clone();
        }
        if state.removed.contains(session_id) {
            return KibitzerSessionResources::detached();
        }
        let resources = KibitzerSessionResources::new();
        state.sessions.insert(session_id.to_string(), resources.clone());
        resources
    }

    /// Shutdown: drop this session's registry ownership, leaving every other session untouched, and mark
    /// it REMOVED so no later lookup recreates it. Returns whether this registry owned the session.
    ///
    /// It only forgets the entry; it does NOT dispose a child or a tool closure, and it is not a licence
    /// to recreate one. The lifecycle owner disposes the actual child FIRST, and the disposed sidecar -
    /// not this registry - is what refuses a late offer (upstream `kibitzer/sidecar.ts` ends a bound
    /// session's lifecycle at `shutdown`). The terminal mark is the second guard: a later lookup for the
    /// removed session gets an inert handle whose budget refuses every call, and whose `reset` cannot
    /// re-enable it.
    pub fn remove(&self, session_id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let owned = state.sessions.remove(session_id).is_some();
        state.removed.insert(session_id.to_string());
        owned
    }
}
