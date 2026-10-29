//! Test child for the facts applied-recovery tests (pin `facts-applied-reconcile-child.ts`): a fresh
//! process running the facts extractor's own session-start reconcile over an identity whose
//! onboarding apply was interrupted. Usage: `facts_applied_reconcile_child <root>`; exits 0 when the
//! reconcile found nothing left to do, 1 otherwise, 2 without a root.
use std::sync::Arc;

use maho_omo_memory::facts_runner::{FactsExtractorRunner, NativeFactsExtractorPort};
use maho_omo_memory::facts_wiring::FactsExtractorPort;
use memory_core::identity::{layout::build_identity_paths, resolve::MemoryIdentity};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // nextest enumerates every test target with `--list`; this helper has no tests to report.
    if std::env::args().any(|arg| arg == "--list") {
        return;
    }
    let Some(root) = std::env::args().nth(1) else {
        std::process::exit(2)
    };
    let root = std::path::PathBuf::from(root);
    let identity = MemoryIdentity {
        id: "apply-agent".into(),
        safe_slug: "apply-agent".into(),
        paths: build_identity_paths(&root, "apply-agent"),
    };
    let runner = Arc::new(FactsExtractorRunner::new(
        identity,
        Arc::new(|| chrono::Utc::now().timestamp_millis()),
    ));
    let people = memory_core::facts::person_routing::FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    // No model registry resolves here (upstream `resolveModelRegistry: () => undefined`), so an
    // attempt that reached the model step would fail; reconcile alone must settle the run.
    let mut port = NativeFactsExtractorPort {
        runner,
        attempt: Arc::new(|_, _| {
            Box::pin(async { Err("no model registry in the reconcile child".to_string()) })
        }),
        people,
        warn: Arc::new(|message| eprintln!("{message}")),
    };
    std::process::exit(match port.reconcile_pending(None).await {
        Ok(()) => 0,
        Err(_) => 1,
    });
}
