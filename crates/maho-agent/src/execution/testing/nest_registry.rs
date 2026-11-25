//! Port of senpi `packages/agent/src/execution/testing/nest-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

const NEST_IMPL: &str = "packages/agent/src/execution/testing/nest-cases.ts";
const CANCEL_IMPL: &str = "packages/agent/src/execution/testing/cancel-cases.ts";
const KILL_NEST_IMPL: &str = "packages/agent/src/execution/testing/kill-nest.ts";

fn nest_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#5669",
        adapters: &[AdapterKind::Store, AdapterKind::Executor],
        expected,
    }
}

pub fn nest_registry() -> Vec<RegisteredCase> {
    vec![
        nest_case(
            "NEST-01",
            NEST_IMPL,
            "a nested request from another hand or naming a forged parent is refused",
        ),
        nest_case(
            "NEST-02",
            NEST_IMPL,
            "a replayed nested request returns the same child, after a restart too; debited once",
        ),
        nest_case(
            "NEST-03",
            NEST_IMPL,
            "sibling children over the parent's budget are refused at the reservation",
        ),
        nest_case(
            "NEST-04",
            NEST_IMPL,
            "a request naming another generation or scope of the parent is refused",
        ),
        nest_case(
            "NEST-05",
            NEST_IMPL,
            "a preflight cross-plane call is admitted under the preparation-phase nested set only",
        ),
        nest_case(
            "NEST-06",
            NEST_IMPL,
            "release widens the nested set to the body phase",
        ),
        nest_case(
            "NEST-07",
            NEST_IMPL,
            "a detached cell is admitted against its registered activity, within its subset, and refused after it ends",
        ),
        nest_case(
            "NEST-08",
            NEST_IMPL,
            "an activity's children debit its own budget and the parent's reservation",
        ),
        nest_case(
            "NEST-09",
            NEST_IMPL,
            "the nested set expires with the parent's lease and renewals, never past its lifetime",
        ),
        nest_case(
            "CANCEL-01",
            CANCEL_IMPL,
            "a parent cancel committed first refuses the child parent_terminal",
        ),
        nest_case(
            "CANCEL-02",
            CANCEL_IMPL,
            "a child admitted first exists; the parent cancel durably cascades to it until delivered",
        ),
        nest_case(
            "KILL-NEST-01",
            KILL_NEST_IMPL,
            "a brain killed before a child admission is durable admits it once on retry",
        ),
        nest_case(
            "KILL-NEST-02",
            KILL_NEST_IMPL,
            "a brain killed after a child admission is durable returns the same child",
        ),
        nest_case(
            "KILL-NEST-03",
            KILL_NEST_IMPL,
            "a brain killed around the cancel cascade re-sends it until recorded",
        ),
    ]
}
