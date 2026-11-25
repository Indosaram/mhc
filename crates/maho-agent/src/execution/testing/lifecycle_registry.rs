//! Port of senpi `packages/agent/src/execution/testing/lifecycle-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

const LIFECYCLE_IMPL: &str = "packages/agent/src/execution/testing/lifecycle-cases.ts";
const SCOPE_IMPL: &str = "packages/agent/src/execution/testing/scope-cases.ts";
const MULTI_LIFE_IMPL: &str = "packages/agent/src/execution/testing/multi-lifecycle-cases.ts";

fn lifecycle_case(
    id: &'static str,
    impl_path: &'static str,
    expected: &'static str,
) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#5681",
        adapters: &[AdapterKind::Store, AdapterKind::Executor],
        expected,
    }
}

pub fn lifecycle_registry() -> Vec<RegisteredCase> {
    vec![
        lifecycle_case(
            "LIFECYCLE-01",
            LIFECYCLE_IMPL,
            "a call after a lifecycle event runs only once the event's effect committed",
        ),
        lifecycle_case(
            "LIFECYCLE-02",
            LIFECYCLE_IMPL,
            "only the scope's issuer submits lifecycle events or closes it",
        ),
        lifecycle_case(
            "LIFECYCLE-03",
            LIFECYCLE_IMPL,
            "a closed scope admits no lifecycle event and is not reopened",
        ),
        lifecycle_case(
            "SCOPE-01",
            SCOPE_IMPL,
            "the idle TTL never closes a scope that holds live resources",
        ),
        lifecycle_case(
            "SCOPE-02",
            SCOPE_IMPL,
            "a forced close writes every live resource's outcome before the scope closes",
        ),
        lifecycle_case(
            "MULTI-LIFE-01",
            MULTI_LIFE_IMPL,
            "A's agent_end and turn_start effects stay in A; B is untouched",
        ),
    ]
}
