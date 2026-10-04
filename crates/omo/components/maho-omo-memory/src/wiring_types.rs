use crate::{context::MemoryIdentityContext,wiring_runtime::MemoryRuntimeWiring};
pub struct MemoryAfterBindInput<'a>{
    pub session_id:&'a str,pub identity:MemoryIdentityContext,pub entries:&'a [serde_json::Value],pub now_ms:i64,
}
pub struct MemoryWiringOptions{pub runtime:MemoryRuntimeWiring,pub skills_usage:crate::skills_usage_wiring::SkillsUsageTrackers}
pub type NativeFactsWiringFactory=std::sync::Arc<dyn Fn(&MemoryIdentityContext,&maho_ext_api::ExtensionContext)->Result<crate::facts_wiring::MemoryFactsWiringOptions,String>+Send+Sync>;
pub type NativeMemoryReconcile=std::sync::Arc<dyn Fn(MemoryIdentityContext)->crate::facts_wiring::FactsExtractorWork+Send+Sync>;
pub struct NativeMemoryWiringOptions{
    pub facts:NativeFactsWiringFactory,pub reconcile:NativeMemoryReconcile,
    pub now:std::sync::Arc<dyn Fn()->f64+Send+Sync>,pub warn:std::sync::Arc<dyn Fn(&str)+Send+Sync>,
}
