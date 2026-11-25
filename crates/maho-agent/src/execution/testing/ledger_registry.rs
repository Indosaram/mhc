//! Port of senpi `packages/agent/src/execution/testing/ledger-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};
use crate::durable::store::StoreWritePoint;

use super::kill_ledger::kill_ledger_declarations;

const EXPECTATIONS: &[&str] = &[
    "identity and generation are bound; replay never re-admits",
    "effect_possible is durable before preparation effects and freeze",
    "consent waits on the frozen call; unauthorized approval is refused",
    "dispatch requires release and is granted at most once",
    "cancel-first stays terminal; late result is audit-only",
    "result-first stays terminal; terminal release, abort and consent are refused",
    "renewal is issuer-only, monotonic, capped and returns terminal outcomes",
    "ACK is issuer-only, terminal-only and idempotent; replay after ACK is result_expired",
    "preparing never reruns after restart; uncertainty follows effect_possible",
    "prepared, consent-waiting and released calls survive restart",
    "reconcilable dispatch has one bounded grace; an authoritative result resolves uncertainty",
    "lease expiry retains its reason; concurrent admissions and owner epochs are fenced",
];

const IDS: &[&str] = &[
    "LEDGER-01",
    "LEDGER-02",
    "LEDGER-03",
    "LEDGER-04",
    "LEDGER-05",
    "LEDGER-06",
    "LEDGER-07",
    "LEDGER-08",
    "LEDGER-09",
    "LEDGER-10",
    "LEDGER-11",
    "LEDGER-12",
];

fn point_label(point: StoreWritePoint) -> String {
    match point {
        StoreWritePoint::Before { .. } => "before".to_string(),
        StoreWritePoint::Write { index, .. } => format!("write {index}"),
        StoreWritePoint::Durable { .. } => "durable".to_string(),
    }
}

pub fn ledger_registry() -> Vec<RegisteredCase> {
    let mut out = Vec::with_capacity(EXPECTATIONS.len() + 64);
    for (index, expected) in EXPECTATIONS.iter().enumerate() {
        out.push(RegisteredCase {
            id: IDS[index],
            impl_path: if index < 6 {
                "packages/agent/src/execution/testing/ledger-cases.ts"
            } else {
                "packages/agent/src/execution/testing/ledger-recovery-cases.ts"
            },
            intro: "#5004",
            adapters: &[AdapterKind::Store],
            expected,
        });
    }
    for decl in kill_ledger_declarations() {
        let expected_str = format!(
            "everywhere: kill {} {}; journal and row recover atomically, with no repeated dispatch",
            point_label(decl.point),
            decl.operation
        );
        let leaked: &'static str = Box::leak(expected_str.into_boxed_str());
        out.push(RegisteredCase {
            id: decl.id,
            impl_path: "packages/agent/src/execution/testing/kill-ledger.ts",
            intro: "#5004",
            adapters: &[AdapterKind::Store],
            expected: leaked,
        });
    }
    out
}
