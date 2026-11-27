//! Port of senpi `packages/agent/src/execution/testing/epoch-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

use super::kill_epoch::kill_epoch_declarations;

const EPOCH_IMPL: &str = "packages/agent/src/execution/testing/epoch-cases.ts";

fn epoch_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#5729",
        adapters: &[
            AdapterKind::Store,
            AdapterKind::Transport,
            AdapterKind::Executor,
        ],
        expected,
    }
}

pub fn epoch_registry() -> Vec<RegisteredCase> {
    let mut out = vec![
        epoch_case(
            "EPOCH-01",
            EPOCH_IMPL,
            "a barrier holding the floor ACK while the new owner dispatches: 0 effects, then once",
        ),
        epoch_case(
            "EPOCH-02",
            EPOCH_IMPL,
            "two concurrent CAS claims: exactly one owner; only its calls run",
        ),
        epoch_case(
            "EPOCH-03",
            EPOCH_IMPL,
            "an old owner's delayed prepare, release or invoke after the new owner's first call is refused",
        ),
        epoch_case(
            "MULTI-EPOCH-01",
            "packages/agent/src/execution/testing/multi-epoch-cases.ts",
            "one session's epoch bump leaves the other sessions' scopes on the hand untouched",
        ),
    ];
    for decl in kill_epoch_declarations() {
        out.push(epoch_case(
            decl.id,
            "packages/agent/src/execution/testing/kill-epoch.ts",
            decl.expected,
        ));
    }
    out
}
