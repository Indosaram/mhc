//! The sidecar's live `nudge` tool (latest `kibitzer/tools/nudge.ts`).
//!
//! The kibitzer judge's ONLY output channel, as a live wrapper over ONE bound session's shared
//! resources. Upstream `createKibitzerSidecarNudgeTool` re-binds the base nudge tool
//! (`kibitzer/nudge-tool.ts`, ported as [`crate::kibitzer_nudge_tool`]) at CALL time to the lifetime
//! allowed set `offered ∪ searched`, the session-scoped `surfaced` set and the CURRENT wake's
//! `accepted` list, and charges the wake budget exactly once. This module is that wrapper; the base
//! owns the rejection order, the exact texts and the `terminate` flag, and holds no per-wake state.
//!
//! # Charge ownership (one charge, never two)
//!
//! Upstream wraps the closure in `budgeted(input.budget, ...)` INSIDE the factory, so the charge
//! belongs to the nudge wrapper itself. The other member tools' ports moved that charge to their
//! registered `ToolDefinition` (see `kibitzer_tools_memory::execute_memory` and the CLI host tools),
//! but the nudge tool keeps it here: [`KibitzerSidecarNudgeTool::execute`] resolves
//! `resources.budget_slot.current()` and charges ONCE on entry, exactly as upstream's `budgeted`. An
//! exhausted budget returns the shared structured rejection
//! (`KibitzerRejectionCode::ToolBudgetExceeded`, `terminate = false`) and appends nothing, so the
//! registered caller MUST NOT charge again.
//!
//! # Call-time resolution (no rebuild, no snapshot at construction)
//!
//! Nothing is captured at construction except the shared [`KibitzerSessionResources`] handle (cloning
//! it shares the slots; it never copies the current accepted list or the current budget). Every call
//! resolves the live slots:
//!
//! * `candidates` - the union of `resources.offered` and `resources.searched`, so a path searched
//!   after construction is eligible without rebuilding the tool. The base validates against a
//!   concrete `&BTreeSet<String>`, so the union is materialized per call as a CALL-TIME SNAPSHOT.
//!   Whether that snapshot is equivalent to upstream's lazy `UnionPathSet` under a concurrent
//!   mutation is NOT claimed here; it awaits lifecycle/registered proof.
//! * `surfaced` - `resources.surfaced`, locked for `&BTreeSet<String>`; main-session scoped.
//! * `max_items` - `resources.max_items()`, the session's `memory.recall.max_items`. It is the
//!   resource's own bound set by the child factory through `bind_max_items`; the wrapper adds NO new
//!   slot and never caches it.
//! * `accepted` - `resources.accepted.current()`, the CURRENT wake's list; a steer that joins a
//!   running turn keeps the same list, and a new-turn admission swaps it (the swap is seen here).
//! * `budget` - `resources.budget_slot.current()`, the CURRENT wake's budget, reset on admission.
//!
//! A REMOVED session's inert handle carries a terminal, always-exhausted budget slot, so every call
//! is refused before it can touch the live sets and no `reset` can re-enable charging.
//!
//! There is no process-global registry and no second native tool surface: this wrapper binds the
//! SAME per-session handle the sidecar and the other four member tools share.

use std::collections::BTreeSet;
use std::sync::PoisonError;

use crate::kibitzer_nudge_tool::{KibitzerNudgeInput, KibitzerNudgeParams, execute_nudge};
use crate::kibitzer_session_resources::KibitzerSessionResources;
use crate::kibitzer_tools_result::{KibitzerRejectionCode, KibitzerToolResult, rejection};

/// The registered tool name (`nudge`), re-exported from the base so the registry reads the metadata
/// from the wrapper it registers (upstream `export { KIBITZER_NUDGE_TOOL_NAME }`).
pub use crate::kibitzer_nudge_tool::KIBITZER_NUDGE_TOOL_NAME;
/// The upstream tool label (`Kibitzer`), re-exported from the base (`{...template}`).
pub use crate::kibitzer_nudge_tool::KIBITZER_NUDGE_TOOL_LABEL;
/// The tool description, re-exported from the base (`{...template}`).
pub use crate::kibitzer_nudge_tool::KIBITZER_NUDGE_DESCRIPTION;
/// The `nudge` parameter schema, re-exported from the base (`{...template}`).
pub use crate::kibitzer_nudge_tool::kibitzer_nudge_parameters;

/// The live `nudge` tool for ONE bound session: the base nudge tool re-bound at call time to the
/// session's shared resources.
pub struct KibitzerSidecarNudgeTool {
    resources: KibitzerSessionResources,
}

/// Builds the live `nudge` tool for ONE bound session (upstream `createKibitzerSidecarNudgeTool`).
///
/// The handle is the SAME [`KibitzerSessionResources`] the sidecar and every other member tool of the
/// child share (one budget slot, one accepted slot, one offered/searched/surfaced set per session);
/// cloning it shares the slots. Build ONE per child from the registry getter; never a second one.
pub fn create_kibitzer_sidecar_nudge_tool(
    resources: KibitzerSessionResources,
) -> KibitzerSidecarNudgeTool {
    KibitzerSidecarNudgeTool { resources }
}

impl KibitzerSidecarNudgeTool {
    /// One `nudge` call: charge the wake budget ONCE, then validate against the live slots.
    ///
    /// The charge is upstream `budgeted`'s: an exhausted budget returns the shared structured
    /// rejection (code `tool_budget_exceeded`, `terminate = false`) and appends nothing. Otherwise the
    /// base runs over the live `offered ∪ searched`, the session `surfaced` set, the session
    /// `max_items` and the CURRENT wake's `accepted` list, and its result - `terminate` included - is
    /// returned unchanged. `params` is the decoded `nudge` call; the registry owns decoding.
    pub fn execute(&self, params: &KibitzerNudgeParams) -> KibitzerToolResult {
        let budget = self.resources.budget_slot.current();
        if !budget.charge() {
            return rejection(
                KibitzerRejectionCode::ToolBudgetExceeded,
                &format!(
                    "The tool-call budget for this wake ({}) is exhausted; end the turn.",
                    budget.limit()
                ),
                None,
            );
        }
        // Materialize the live allowed set (`offered ∪ searched`) as a call-time snapshot. Lock order:
        // the `offered` and `searched` guards are each taken and RELEASED before the `surfaced` guard
        // is acquired; `surfaced` is then held while the accepted slot is resolved
        // (`accepted.current()` takes the slot's own inner lock and returns its Arc) and the accepted
        // guard is taken from that Arc.
        let offered: BTreeSet<String> =
            self.resources.offered.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let searched: BTreeSet<String> =
            self.resources.searched.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let candidates: BTreeSet<String> = offered.union(&searched).cloned().collect();
        let surfaced = self.resources.surfaced.lock().unwrap_or_else(PoisonError::into_inner);
        let accepted = self.resources.accepted.current();
        let mut accepted = accepted.lock().unwrap_or_else(PoisonError::into_inner);
        execute_nudge(
            KibitzerNudgeInput {
                candidates: &candidates,
                surfaced: &*surfaced,
                max_items: self.resources.max_items(),
                accepted: &mut *accepted,
            },
            params,
        )
    }
}
