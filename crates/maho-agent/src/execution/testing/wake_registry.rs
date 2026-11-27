//! Port of senpi `packages/agent/src/execution/testing/wake-registry.ts`.

use crate::conformance::registry::{AdapterKind, RegisteredCase};

const WAKE_EXEC_IMPL: &str = "packages/agent/src/execution/testing/wake-exec-cases.ts";
const KILL_WAKEX_IMPL: &str = "packages/agent/src/execution/testing/kill-wakex.ts";

fn wake_case(id: &'static str, impl_path: &'static str, expected: &'static str) -> RegisteredCase {
    RegisteredCase {
        id,
        impl_path,
        intro: "#5675",
        adapters: &[AdapterKind::Store, AdapterKind::Executor],
        expected,
    }
}

pub fn wake_exec_registry() -> Vec<RegisteredCase> {
    vec![
        wake_case(
            "WAKEX-01",
            WAKE_EXEC_IMPL,
            "a wake is durable on the hand, committed on the brain, acked, taken once",
        ),
        wake_case(
            "WAKEX-02",
            WAKE_EXEC_IMPL,
            "a re-offered wake whose ack was lost is deduplicated by event id and acked",
        ),
        wake_case(
            "WAKEX-03",
            WAKE_EXEC_IMPL,
            "un-acked wakes are re-offered on reconnect in offer order",
        ),
        wake_case(
            "WAKEX-04",
            WAKE_EXEC_IMPL,
            "a wake refused for its scope commits nothing, is never acked and is not re-offered",
        ),
        wake_case(
            "WAKEX-05",
            WAKE_EXEC_IMPL,
            "an un-acked wake lives until scope close plus 7 days, then wake_expired",
        ),
        wake_case(
            "WAKEX-06",
            WAKE_EXEC_IMPL,
            "a closed scope dedups re-offers within retention and refuses late or new wakes",
        ),
        wake_case(
            "WAKEX-07",
            WAKE_EXEC_IMPL,
            "concurrent drains hand each wake to the loop exactly once",
        ),
        wake_case(
            "WAKEX-08",
            WAKE_EXEC_IMPL,
            "a wake refused before its scope binds is re-offered and delivered once it binds",
        ),
        wake_case(
            "KILL-WAKEX-01",
            KILL_WAKEX_IMPL,
            "a hand killed before the outbox commit leaves nothing to re-offer",
        ),
        wake_case(
            "KILL-WAKEX-02",
            KILL_WAKEX_IMPL,
            "a hand killed after the outbox commit re-offers the wake once",
        ),
        wake_case(
            "KILL-WAKEX-03",
            KILL_WAKEX_IMPL,
            "a brain killed before the inbox commit sent no ack; the re-offer delivers once",
        ),
        wake_case(
            "KILL-WAKEX-04",
            KILL_WAKEX_IMPL,
            "a brain killed after the inbox commit dedups the re-offer and acks it",
        ),
        wake_case(
            "KILL-WAKEX-05",
            KILL_WAKEX_IMPL,
            "a hand killed before recording the ack re-offers; the duplicate is acked",
        ),
        wake_case(
            "KILL-WAKEX-06",
            KILL_WAKEX_IMPL,
            "a brain killed before recording a take hands the same wake over again",
        ),
    ]
}
