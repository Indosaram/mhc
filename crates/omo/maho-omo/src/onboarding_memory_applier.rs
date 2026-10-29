//! Onboarding's `onboarding_memory_apply` -> the memory component's `omo.memory.facts.apply` applier.
//! Onboarding registers before memory, so the published applier is looked up on every call.

use maho_omo_onboarding::tui_memory_runtime::{MemoryFactsApplier, MemoryFactsApplyResponse};
use memory_core::facts::extraction::FactsExtractionRecord;

pub struct PublishedMemoryFactsApplier;

impl MemoryFactsApplier for PublishedMemoryFactsApplier {
    fn apply(
        &self,
        records: Vec<FactsExtractionRecord>,
        session_id: Option<String>,
    ) -> MemoryFactsApplyResponse {
        let Some(applier) = maho_omo_memory::facts_apply_rpc::session_facts_applier() else {
            return MemoryFactsApplyResponse::Failed {
                reason: "Memory is off in this session, so nothing was written.".into(),
            };
        };
        let request = serde_json::json!({ "records": records });
        // Called from onboarding's spawn_blocking thread, so blocking on the runtime handle is allowed.
        let response = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(applier(request, session_id)),
            Err(error) => Err(format!("The memory apply runtime could not load: {error}")),
        };
        response_of(response)
    }
}

fn response_of(response: Result<serde_json::Value, String>) -> MemoryFactsApplyResponse {
    let value = match response {
        Ok(value) => value,
        Err(reason) => return MemoryFactsApplyResponse::Failed { reason },
    };
    let affected_paths = || -> Vec<String> {
        value["affectedPaths"]
            .as_array()
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    match value["outcome"].as_str() {
        Some("committed") => MemoryFactsApplyResponse::Committed {
            sha: value["sha"].as_str().unwrap_or_default().to_owned(),
            affected_paths: affected_paths(),
        },
        Some(outcome) => MemoryFactsApplyResponse::Outcome {
            outcome: outcome.to_owned(),
            affected_paths: affected_paths(),
        },
        None => MemoryFactsApplyResponse::Failed {
            reason: value["reason"]
                .as_str()
                .unwrap_or("The memory apply answered without an outcome.")
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_applier_answer() {
        assert_eq!(
            response_of(Ok(
                serde_json::json!({"outcome": "committed", "sha": "abc", "affectedPaths": ["a.md"], "batchId": "b"})
            )),
            MemoryFactsApplyResponse::Committed {
                sha: "abc".into(),
                affected_paths: vec!["a.md".into()]
            }
        );
        assert_eq!(
            response_of(Ok(
                serde_json::json!({"outcome": "parent_dirty", "batchId": "b"})
            )),
            MemoryFactsApplyResponse::Outcome {
                outcome: "parent_dirty".into(),
                affected_paths: vec![]
            }
        );
        assert_eq!(
            response_of(Ok(serde_json::json!({"kind": "invalid", "reason": "bad"}))),
            MemoryFactsApplyResponse::Failed {
                reason: "bad".into()
            }
        );
        assert_eq!(
            response_of(Err("boom".into())),
            MemoryFactsApplyResponse::Failed {
                reason: "boom".into()
            }
        );
    }
}
