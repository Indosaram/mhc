//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-grants.ts`.
//!
//! Presenter grant feature negotiation (`secret_input_v1`, `elsewhere`), grant liveness checks,
//! and secret-candidate filtering.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SECRET_INPUT_FEATURE: &str = "secret_input_v1";
pub const ELSEWHERE_FEATURE: &str = "elsewhere";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenterGrantOptions {
    pub kind: String,
    #[serde(default)]
    pub features: Vec<String>,
}

/// Validates and extracts supported presenter grant feature tags.
pub fn parse_features(value: Option<&Value>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str()?;
        if !matches!(s, SECRET_INPUT_FEATURE | ELSEWHERE_FEATURE) {
            return None;
        }
        if !out.iter().any(|existing| existing == s) {
            out.push(s.to_owned());
        }
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantCandidate {
    pub grant_id: String,
    pub secret_input: bool,
    pub elsewhere: bool,
    pub is_account_principal: bool,
    pub active: bool,
}

pub fn is_secret_input_held(candidates: &[GrantCandidate]) -> bool {
    candidates
        .iter()
        .any(|g| g.active && g.secret_input && g.is_account_principal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_features_and_secret_candidates() {
        assert_eq!(parse_features(None), Some(Vec::new()));
        assert_eq!(
            parse_features(Some(&json!(["secret_input_v1", "elsewhere"]))),
            Some(vec!["secret_input_v1".to_owned(), "elsewhere".to_owned()])
        );
        assert_eq!(parse_features(Some(&json!(["unknown"]))), None);
        assert!(is_secret_input_held(&[GrantCandidate {
            grant_id: "g1".to_owned(),
            secret_input: true,
            elsewhere: false,
            is_account_principal: true,
            active: true,
        }]));
        assert!(!is_secret_input_held(&[GrantCandidate {
            grant_id: "g2".to_owned(),
            secret_input: true,
            elsewhere: false,
            is_account_principal: false,
            active: true,
        }]));
    }
}
