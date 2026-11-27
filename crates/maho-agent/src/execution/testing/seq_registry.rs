//! Port of senpi `packages/agent/src/execution/testing/seq-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

const INTRO: &str = "#5661";

pub fn seq_registry() -> Vec<RegisteredCase> {
    vec![
        RegisteredCase {
            id: "SEQ-01",
            impl_path: "packages/agent/src/execution/testing/seq-cases.ts",
            intro: INTRO,
            adapters: &[AdapterKind::Store, AdapterKind::Executor],
            expected: "sequential mode interleaves prepare and run per call across executors; a crash resumes without rerunning; a stop ends the run unprepared",
        },
        RegisteredCase {
            id: "MULTI-FAIR-02",
            impl_path: "packages/agent/src/execution/testing/multi-seq-cases.ts",
            intro: INTRO,
            adapters: &[AdapterKind::Store, AdapterKind::Executor],
            expected: "a long sequential batch in session A does not starve session B on the same executor",
        },
        RegisteredCase {
            id: "CONSENT-01",
            impl_path: "packages/agent/src/execution/testing/consent-cases.ts",
            intro: "#5663",
            adapters: &[AdapterKind::Store, AdapterKind::Executor],
            expected: "cross-machine only: the lease expires while a call awaits Gate B consent: lease_expired, uncertain, the batch ABORTed, no successor",
        },
        RegisteredCase {
            id: "CONSENT-02",
            impl_path: "packages/agent/src/execution/testing/consent-cases.ts",
            intro: "#5663",
            adapters: &[AdapterKind::Store, AdapterKind::Executor],
            expected: "cross-machine only: an approval that arrives after the lease expired is rejected; the outcome stays lease_expired",
        },
    ]
}
