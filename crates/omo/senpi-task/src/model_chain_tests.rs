use std::collections::BTreeSet;

use pretty_assertions::assert_eq;

use super::*;
use crate::delegate_adapter::fallback_entry;

#[test]
fn available_candidates_strip_provider_prefix_before_transformation() {
    let chain = vec![fallback_entry(&["openai"], "openai/gpt-5.6-sol", Some("high"))];
    let available = BTreeSet::from(["openai/gpt-5.6-sol".to_string()]);
    assert_eq!(
        available_chain_candidates(&chain, &available),
        vec![ModelChainCandidate {
            model: "openai/gpt-5.6-sol".into(),
            variant: Some("high".into()),
            reasoning_effort: None,
        }]
    );
}
