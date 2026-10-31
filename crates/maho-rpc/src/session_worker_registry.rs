//! senpi `modes/rpc/session-worker-registry.ts`: the registry a session worker runs its one session in.
//!
//! The worker owns its process's runtime, so the session file loads in one pass: slicing it (senpi#3518) only
//! helps a loop other sessions share, and a worker blocked in a native read is what the host's quarantine and
//! ownership rules are built to handle. Its host owns the acceptance lane, so the isolate opens none.
//!
//! NATIVE-DIVERGENCE(senpi `agentDir`, `createRuntime`, `sessionFileLoad: "blocking"`): the native registry
//! takes its runtime factory per open (`create_admitted_runtime`) and always loads the file in one pass, so
//! only the worker-isolate marker is carried here.

/// senpi `createSessionWorkerRegistry`.
pub fn create_session_worker_registry() -> crate::session_registry::RpcSessionRegistry {
    crate::session_registry::RpcSessionRegistry {
        worker_isolate: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_registry_is_marked_as_the_isolate_and_starts_empty() {
        let registry = create_session_worker_registry();
        assert!(registry.worker_isolate);
        assert_eq!(registry.size(), 0);
    }
}
