pub use super::completion_contracts::*;
pub use super::completion_delivery::*;
pub use super::completion_records::*;
use std::path::Path;

pub fn record_reflection_completion(dir: &Path, record: &ReflectionCompletionRecord, live: Option<&mut dyn ReflectionLiveSession>, now_ms: i64) -> Result<ReflectionCompletionRecord, CompletionRecordError> {
    let durable = ensure_reflection_completion(dir, record)?;
    let Some(live) = live else { return Ok(durable); };
    if durable.delivery.status == DeliveryStatus::Consumed { return Ok(durable); }
    let delivered = deliver_reflection_completion(dir, &durable, live, true, now_ms)?;
    live.on_completion(&delivered.run_id);
    Ok(delivered)
}
