//! Port of senpi `packages/agent/src/execution/testing/batch-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

use super::kill_batch::kill_batch_declarations;

const BATCH_CASES_IMPL: &str = "packages/agent/src/execution/testing/batch-cases.ts";
const INTRO: &str = "#5416";

fn batch_case(id: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path: BATCH_CASES_IMPL,
        intro: INTRO,
        adapters: &[AdapterKind::Store, AdapterKind::Executor],
        expected,
    }
}

pub fn batch_registry() -> Vec<RegisteredCase> {
    let mut out = vec![
        batch_case(
            "BATCH-01",
            "the brain crashes after the manifest: the same execution ids are re-prepared; nothing had run",
        ),
        batch_case(
            "BATCH-02",
            "the brain crashes after the prepares: prepare is re-sent, stored replies return, hooks never rerun",
        ),
        batch_case(
            "BATCH-03",
            "the brain crashes inside a released wave: released calls get a re-sent release, unmarked ready calls are released, the next wave waits",
        ),
        batch_case(
            "BATCH-05",
            "an executor restart keeps prepared and released calls, and release proceeds",
        ),
        batch_case(
            "BATCH-06",
            "a lost prepared execution answers unknown_execution and is uncertain; siblings settle",
        ),
        batch_case(
            "BATCH-07",
            "prepare runs in source order across executors; each release waits for its dependencies' final outcomes",
        ),
        batch_case(
            "BATCH-08",
            "a batch_abort reply ABORTs the batch: no later prepare, no body, abort re-sent on recovery",
        ),
        batch_case(
            "BATCH-09",
            "a blocked call is a per-call result; the other calls are released",
        ),
        batch_case(
            "REPLAY-01",
            "an executor restart during preparation replays like today (X12): placeholder result, never rerun, a reissued call runs once",
        ),
        batch_case(
            "REPLAY-02",
            "the same beside a prepared sibling: the sibling is aborted unrun, both replay with the placeholder",
        ),
    ];
    for decl in kill_batch_declarations() {
        let expected_str = format!(
            "everywhere: kill the brain {} its {} write ({}); recovery reaches the uncrashed outcomes with stable ids, each step run once",
            decl.kill.at.as_str(),
            decl.kill.label,
            decl.scenario
        );
        let leaked: &'static str = Box::leak(expected_str.into_boxed_str());
        out.push(RegisteredCase {
            id: decl.id,
            impl_path: "packages/agent/src/execution/testing/kill-batch.ts",
            intro: INTRO,
            adapters: &[AdapterKind::Store, AdapterKind::Executor],
            expected: leaked,
        });
    }
    out
}
