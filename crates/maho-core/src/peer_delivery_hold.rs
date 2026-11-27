//! Port of senpi packages/coding-agent/src/core/peer-delivery-hold.ts.

use std::collections::BTreeSet;

use crate::engine_turn_hold::{HoldKind, SessionHold};

/// The holds that keep a message another session sent from starting a turn (`senpi#6490`), sorted;
/// empty when it may start.
pub fn holds_keeping_peer_delivery(holds: &[SessionHold], interrupted_run: bool) -> Vec<HoldKind> {
    let mut has_non_unattended = interrupted_run;
    let mut has_unattended = false;
    let mut has_session_hold = interrupted_run;
    let mut has_inherited_hold = false;
    for hold in holds {
        match hold.kind {
            HoldKind::RunFence => {}
            HoldKind::Unattended => has_unattended = true,
            HoldKind::SessionHold => {
                has_session_hold = true;
                has_non_unattended = true;
            }
            HoldKind::InheritedHold => {
                has_inherited_hold = true;
                has_non_unattended = true;
            }
        }
    }
    if !has_non_unattended {
        return Vec::new();
    }
    let mut kinds = BTreeSet::new();
    // Sort alphabetically by wire string (`inherited_hold`, `session_hold`, `unattended`) to match TS `sort()`.
    if has_inherited_hold {
        kinds.insert(("inherited_hold", HoldKind::InheritedHold));
    }
    if has_session_hold {
        kinds.insert(("session_hold", HoldKind::SessionHold));
    }
    if has_unattended {
        kinds.insert(("unattended", HoldKind::Unattended));
    }
    kinds.into_iter().map(|(_, k)| k).collect()
}
