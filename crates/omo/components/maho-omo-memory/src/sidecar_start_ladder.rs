//! The candidates one Kibitzer start walks, in order, inside its own recall category (#10536).
//!
//! A start the post-start pin check refuses (`model_unavailable`: the engine opened another model
//! than the pinned one) is not the user's to fix while the category still has another connected
//! candidate, so the starter moves to the next one in the same call and remembers the refused model
//! for the sidecar's life; later wakes then begin on the candidate that answered. The ladder is the
//! resolution's own chain and never reaches beyond the category (#9111 / #9114). When every
//! candidate has been refused, the next start walks the whole chain again so the failure is reported
//! through the gate notice instead of being hidden.

use std::collections::HashSet;

use crate::memory_child_model_chain::{child_model_chain_spec, ChildModelChainInput, ChildModelChainSpec};
use crate::worker::memory_model_attempts::ReflectionModelCandidate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KibitzerStartCandidate {
    pub model: String,
    pub thinking: Option<String>,
    pub chain: ChildModelChainSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KibitzerStartResolution {
    pub model: String,
    pub thinking: Option<String>,
    pub fallbacks: Vec<ReflectionModelCandidate>,
}

pub fn kibitzer_start_ladder(
    resolution: &KibitzerStartResolution,
    refused: &HashSet<String>,
) -> Vec<KibitzerStartCandidate> {
    let head = ReflectionModelCandidate {
        model: resolution.model.clone(),
        thinking: resolution.thinking.clone(),
        load_extensions: None,
    };
    let mut all = vec![head];
    all.extend(resolution.fallbacks.clone());
    let open: Vec<ReflectionModelCandidate> = all
        .iter()
        .filter(|c| !refused.contains(&c.model))
        .cloned()
        .collect();
    let ladder = if !open.is_empty() { open } else { all };
    ladder
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let fallbacks = &ladder[index + 1..];
            let chain = child_model_chain_spec(ChildModelChainInput {
                model: &candidate.model,
                fallbacks,
            });
            KibitzerStartCandidate {
                model: candidate.model.clone(),
                thinking: candidate.thinking.clone(),
                chain,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_the_head_candidate_is_refused_when_ladder_built_then_the_next_candidate_is_first() {
        let resolution = KibitzerStartResolution {
            model: "mock-1".to_string(),
            thinking: None,
            fallbacks: vec![
                ReflectionModelCandidate {
                    model: "mock-2".to_string(),
                    thinking: None,
                    load_extensions: None,
                },
                ReflectionModelCandidate {
                    model: "mock-3".to_string(),
                    thinking: None,
                    load_extensions: None,
                },
            ],
        };
        let mut refused = HashSet::new();
        refused.insert("mock-1".to_string());

        let ladder = kibitzer_start_ladder(&resolution, &refused);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].model, "mock-2");
        assert_eq!(ladder[1].model, "mock-3");
    }

    #[test]
    fn given_every_candidate_is_refused_when_ladder_built_then_it_walks_the_whole_chain_again() {
        let resolution = KibitzerStartResolution {
            model: "mock-1".to_string(),
            thinking: None,
            fallbacks: vec![ReflectionModelCandidate {
                model: "mock-2".to_string(),
                thinking: None,
                load_extensions: None,
            }],
        };
        let mut refused = HashSet::new();
        refused.insert("mock-1".to_string());
        refused.insert("mock-2".to_string());

        let ladder = kibitzer_start_ladder(&resolution, &refused);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].model, "mock-1");
        assert_eq!(ladder[1].model, "mock-2");
    }

    #[test]
    fn given_no_refusals_when_ladder_built_then_all_candidates_in_order() {
        let resolution = KibitzerStartResolution {
            model: "mock-1".to_string(),
            thinking: Some("minimal".to_string()),
            fallbacks: vec![ReflectionModelCandidate {
                model: "mock-2".to_string(),
                thinking: None,
                load_extensions: None,
            }],
        };
        let refused = HashSet::new();

        let ladder = kibitzer_start_ladder(&resolution, &refused);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].model, "mock-1");
        assert_eq!(ladder[0].thinking.as_deref(), Some("minimal"));
        assert_eq!(ladder[1].model, "mock-2");
        assert_eq!(ladder[1].thinking, None);
    }
}
