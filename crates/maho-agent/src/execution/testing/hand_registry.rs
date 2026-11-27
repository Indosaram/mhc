//! Port of senpi `packages/agent/src/execution/testing/hand-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

use super::kill_hand::kill_hand_declarations;

const HAND_INTRO: &str = "#5482";
const KILL_HAND_IMPL: &str = "packages/agent/src/execution/testing/kill-hand.ts";
const MULTI_HAND_IMPL: &str = "packages/agent/src/execution/testing/multi-hand-cases.ts";
const FAULT_HAND_IMPL: &str = "packages/agent/src/execution/testing/fault-hand-cases.ts";

fn hand_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: HAND_INTRO,
        adapters: &[
            AdapterKind::Store,
            AdapterKind::Transport,
            AdapterKind::Executor,
        ],
        expected,
    }
}

pub fn hand_registry() -> Vec<RegisteredCase> {
    let mut out = Vec::new();
    for decl in kill_hand_declarations() {
        out.push(hand_case(decl.id, KILL_HAND_IMPL, decl.expected));
    }
    out.push(hand_case(
        "MULTI-HAND-01",
        MULTI_HAND_IMPL,
        "N scopes batch at once on one hand; each scope's calls start in lane order",
    ));
    out.push(hand_case(
        "MULTI-KILL-01",
        MULTI_HAND_IMPL,
        "cross-machine: a hand killed with several scopes in flight; each ends hand_restarted, uncertain, never rerun",
    ));
    for (index, profile) in ["slow", "lossy", "laggy", "flap", "drop-reattach"]
        .iter()
        .enumerate()
    {
        let id: &'static str = Box::leak(format!("FAULT-HAND-0{}", index + 1).into_boxed_str());
        let expected: &'static str = Box::leak(
            format!("{profile} profile: every call settles with its unfaulted result, runs once, per-scope order kept")
                .into_boxed_str(),
        );
        out.push(hand_case(id, FAULT_HAND_IMPL, expected));
    }
    out.push(hand_case(
        "FAULT-HAND-MULTI-01",
        FAULT_HAND_IMPL,
        "hand unreachable past the resume window: dependent calls pause, then each scope resumes exactly once",
    ));
    out
}
