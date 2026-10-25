pub type ReconcileRunsCallback = Option<std::sync::Arc<dyn Fn(&MemoryIdentityContext) -> Result<(), String> + Send + Sync>>;
use crate::{context::MemoryIdentityContext, wiring_runtime::MemoryRuntimeWiring};
pub struct MemoryAfterBindInput<'a> {
    pub session_id: &'a str,
    pub identity: MemoryIdentityContext,
    pub entries: &'a [serde_json::Value],
    pub now_ms: i64,
}
pub struct MemoryWiringOptions {
    pub runtime: MemoryRuntimeWiring,
    pub skills_usage: crate::skills_usage_wiring::SkillsUsageTrackers,
}
pub type NativeFactsWiringFactory = std::sync::Arc<
    dyn Fn(
            &MemoryIdentityContext,
            &maho_ext_api::ExtensionContext,
        ) -> Result<crate::facts_wiring::MemoryFactsWiringOptions, String>
        + Send
        + Sync,
>;
/// Bind-time status refresh with the advisory check on (pin `wiring-reflection-live.ts` refreshInitialStatus).
pub type NativeInitialStatus =
    std::sync::Arc<dyn Fn(&MemoryIdentityContext, &maho_ext_api::ExtensionContext) + Send + Sync>;
pub type NativeMemoryReconcile = std::sync::Arc<
    dyn Fn(MemoryIdentityContext) -> crate::facts_wiring::FactsExtractorWork + Send + Sync,
>;
#[derive(Clone)]
pub struct NativeMemoryWiringOptions {
    pub facts: NativeFactsWiringFactory,
    pub reconcile: NativeMemoryReconcile,
    pub initial_status: NativeInitialStatus,
    pub now: std::sync::Arc<dyn Fn() -> f64 + Send + Sync>,
    pub warn: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
}
/// Optional seams of the memory wiring (pin `MemoryWiringOptions.createCorpusCache` / `deviceName`).
/// Kept off `MemoryWiringOptions` so its existing struct-literal callers stay valid.
#[derive(Clone, Default)]
pub struct MemoryWiringSeams {
    /// The shared recall corpus cache; tests substitute one whose pending term-store save they control.
    pub create_corpus_cache: Option<
        std::sync::Arc<
            dyn Fn(
                    Option<crate::recall_wiring::RecallWarn>,
                ) -> memory_core::recall::RecallCorpusCache
                + Send
                + Sync,
        >,
    >,
    /// A sync principal's device name for the memory notice's "synced from"; production passes the sync runner's.
    pub device_name: Option<crate::projection_pin::DeviceNameResolver>,
    /// Settles the recall term-store save at session shutdown (the recall wiring's `settled`).
    pub recall_store_settle: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// A memory repository's sync status for the footer and its notice; production passes the sync runner's.
    pub sync_status: Option<crate::sync_status_cached::SyncStatusReader>,
    /// Settles reflection runs for an identity (the runtime's `reconcile`); the stale-holder recovery
    /// drives it when a later turn finds a provably dead lock holder.
    pub reconcile_runs:
        ReconcileRunsCallback,
}

impl MemoryWiringSeams {
    /// The corpus cache the recall wiring uses: the override, else the shared per-repository cache.
    pub fn corpus_cache(
        &self,
        warn: Option<crate::recall_wiring::RecallWarn>,
    ) -> memory_core::recall::RecallCorpusCache {
        match &self.create_corpus_cache {
            Some(create) => create(warn),
            None => crate::recall_wiring::create_shared_recall_corpus_cache(warn),
        }
    }
}
