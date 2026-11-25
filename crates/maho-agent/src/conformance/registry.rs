//! Port of senpi `packages/agent/src/conformance/registry.ts`.

use crate::durable::testing::kill_store::kill_store_declarations;
use crate::durable::testing::kill_wake::kill_wake_declarations;
use crate::execution::testing::batch_registry::batch_registry;
use crate::execution::testing::epoch_registry::epoch_registry;
use crate::execution::testing::hand_registry::hand_registry;
use crate::execution::testing::kill_hook::kill_hook_declarations;
use crate::execution::testing::ledger_registry::ledger_registry;
use crate::execution::testing::lifecycle_registry::lifecycle_registry;
use crate::execution::testing::nest_registry::nest_registry;
use crate::execution::testing::seq_registry::seq_registry;
use crate::execution::testing::wake_registry::wake_exec_registry;

pub const ADAPTER_KINDS: &[AdapterKind] = &[
    AdapterKind::Store,
    AdapterKind::Wake,
    AdapterKind::Transport,
    AdapterKind::Executor,
    AdapterKind::Host,
];

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    Store,
    Wake,
    Transport,
    Executor,
    Host,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Wake => "wake",
            Self::Transport => "transport",
            Self::Executor => "executor",
            Self::Host => "host",
        }
    }
}

/// One conformance case: where it is implemented, which change introduced it, what it needs, and what it proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredCase {
    pub id: &'static str,
    pub impl_path: &'static str,
    pub intro: &'static str,
    pub adapters: &'static [AdapterKind],
    pub expected: &'static str,
}

const STORE_CASES_IMPL: &str = "packages/agent/src/durable/testing/store-cases.ts";
const KILL_STORE_IMPL: &str = "packages/agent/src/durable/testing/kill-store.ts";
const WAKE_CASES_IMPL: &str = "packages/agent/src/durable/testing/wake-cases.ts";
const MULTI_HOST_IMPL: &str = "packages/agent/src/durable/testing/multi-host-cases.ts";
const KILL_WAKE_IMPL: &str = "packages/agent/src/durable/testing/kill-wake.ts";
const TRANSPORT_CASES_IMPL: &str = "packages/agent/src/transport/testing/transport-cases.ts";
const MULTI_CASES_IMPL: &str = "packages/agent/src/transport/testing/multi-cases.ts";
const FAULT_CASES_IMPL: &str = "packages/agent/src/transport/testing/fault-cases.ts";
const KILL_HOOK_IMPL: &str = "packages/agent/src/execution/testing/kill-hook.ts";

fn transport_case(
    id: &'static str,
    impl_path: &'static str,
    expected: &'static str,
) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#4741",
        adapters: &[AdapterKind::Transport],
        expected,
    }
}

fn fault_case(n: usize, profile: &str) -> RegisteredCase {
    let id: &'static str = Box::leak(format!("FAULT-TRANSPORT-0{n}").into_boxed_str());
    let expected: &'static str = Box::leak(
        format!(
            "{profile} profile: lane order kept, committed frames delivered once, resume from the last ACK"
        )
        .into_boxed_str(),
    );
    transport_case(id, FAULT_CASES_IMPL, expected)
}

fn wake_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#4735",
        adapters: &[AdapterKind::Store, AdapterKind::Wake],
        expected,
    }
}

fn store_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#4730",
        adapters: &[AdapterKind::Store],
        expected,
    }
}

pub fn conformance_registry() -> Vec<RegisteredCase> {
    let mut out = Vec::new();
    out.extend(ledger_registry());
    out.extend(batch_registry());
    out.extend(seq_registry());
    out.extend(nest_registry());
    out.extend(wake_exec_registry());
    out.extend(lifecycle_registry());
    out.extend([
        store_case(
            "STORE-01",
            STORE_CASES_IMPL,
            "a commit adopts once, after its batch is durable; every instance sees each batch whole",
        ),
        store_case(
            "STORE-02",
            STORE_CASES_IMPL,
            "a cut at every write point of a commit leaves nothing of it durable, with no adopt and no seq burned",
        ),
        store_case(
            "STORE-03",
            STORE_CASES_IMPL,
            "concurrent admissions run one at a time in admission order; a refused mutation releases the line",
        ),
        store_case(
            "STORE-04",
            STORE_CASES_IMPL,
            "an old owner's delayed commit or claim after the new owner's first commit is refused and changes nothing",
        ),
        store_case(
            "STORE-05",
            STORE_CASES_IMPL,
            "claim + import across hosts is whole, idempotent and tail-only; a diverged log is refused atomically",
        ),
        store_case(
            "STORE-06",
            STORE_CASES_IMPL,
            "minted IDs are unique; record and document scans, deletes and duplicate-ID refusal behave as specified",
        ),
        store_case(
            "STORE-07",
            STORE_CASES_IMPL,
            "a claim raises the epoch by exactly one; a claim that skips an epoch is refused and changes nothing",
        ),
    ]);
    for decl in kill_store_declarations() {
        let point_desc = decl.at;
        let op_desc = match decl.op {
            crate::durable::store::StoreOperation::Commit => "commit",
            crate::durable::store::StoreOperation::Claim => "claim",
            crate::durable::store::StoreOperation::Import => "import",
        };
        let expected: &'static str = Box::leak(
            format!("{} : kill {point_desc} a {op_desc}", decl.placement).into_boxed_str(),
        );
        out.push(store_case(decl.id, KILL_STORE_IMPL, expected));
    }
    out.extend([
        wake_case(
            "WAKE-01",
            WAKE_CASES_IMPL,
            "pre-arm: a recovery alarm is durable before admitted work starts and before a new instance resumes",
        ),
        wake_case(
            "WAKE-02",
            WAKE_CASES_IMPL,
            "update: after each commit the alarm is min(heartbeat if active, earliest wake), cleared only when idle",
        ),
        wake_case(
            "WAKE-03",
            WAKE_CASES_IMPL,
            "resume: a fire re-arms before resume, runs due work once, and an early fire is harmless",
        ),
        wake_case(
            "WAKE-RET-01",
            WAKE_CASES_IMPL,
            "retention: an alarm survives host restarts and reopened instances, fires once, and is cleared after the work",
        ),
        wake_case(
            "MULTI-HOST-01",
            MULTI_HOST_IMPL,
            "many sessions on one host instance: records and wakes stay per session",
        ),
    ]);
    for decl in kill_wake_declarations() {
        let op_desc = match decl.operation {
            crate::durable::testing::kill_wake::KillWakeOperation::Admit => "admit",
            crate::durable::testing::kill_wake::KillWakeOperation::Commit => "commit",
            crate::durable::testing::kill_wake::KillWakeOperation::Update => "update",
            crate::durable::testing::kill_wake::KillWakeOperation::Fire => "fire",
        };
        let expected: &'static str = Box::leak(
            format!("{}: kill {} during {op_desc}", decl.placement, decl.point).into_boxed_str(),
        );
        out.push(wake_case(decl.id, KILL_WAKE_IMPL, expected));
    }
    out.extend([
        transport_case(
            "TRANSPORT-01",
            TRANSPORT_CASES_IMPL,
            "order: frames on each lane are delivered in send order, each once",
        ),
        transport_case(
            "TRANSPORT-02",
            TRANSPORT_CASES_IMPL,
            "resume: after the carrier drops and re-attaches, each lane continues from its last ACK; nothing lost or repeated",
        ),
        transport_case(
            "TRANSPORT-03",
            TRANSPORT_CASES_IMPL,
            "backpressure: a stalled receiver bounds its lane's senders; sends resolve in order once it drains",
        ),
        transport_case(
            "MULTI-LANE-01",
            MULTI_CASES_IMPL,
            "N scopes over one transport keep per-scope order and interleave",
        ),
        transport_case(
            "MULTI-FAIR-01",
            MULTI_CASES_IMPL,
            "a stalled or long lane in A does not starve B: B is delivered within a bounded number of A's frames",
        ),
        transport_case(
            "MULTI-FAIR-03",
            MULTI_CASES_IMPL,
            "a frame lost on one lane is resent on that lane's own deadline: another lane's ACK progress never postpones it",
        ),
        fault_case(1, "slow"),
        fault_case(2, "lossy"),
        fault_case(3, "laggy"),
        fault_case(4, "flap"),
        fault_case(5, "drop-reattach"),
    ]);
    out.extend(hand_registry());
    out.extend(epoch_registry());
    for decl in kill_hook_declarations() {
        out.push(RegisteredCase {
            id: decl.id,
            impl_path: KILL_HOOK_IMPL,
            intro: "#5726",
            adapters: &[AdapterKind::Transport, AdapterKind::Executor],
            expected: decl.expected,
        });
    }
    out
}

pub const CONFORMANCE_REGISTRY: fn() -> Vec<RegisteredCase> = conformance_registry;
