//! Success criteria input parsing and defaults.

use std::collections::BTreeMap;
use serde_json::Value;

use crate::constants::ULW_LOOP_SUCCESS_CRITERION_USER_MODELS;
use crate::domain_types::UlwLoopSuccessCriterion;
use crate::runtime::UlwLoopError;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuccessCriterionInput {
    pub scenario: String,
    pub expected_evidence: String,
    pub user_model: Option<String>,
    pub essential: Option<bool>,
}

pub fn criterion_id(index: usize) -> String {
    format!("C{:03}", index + 1)
}

fn is_user_model(value: &str) -> bool {
    ULW_LOOP_SUCCESS_CRITERION_USER_MODELS.contains(&value)
}

pub fn criteria_from_input(input: &[Value]) -> Result<Vec<UlwLoopSuccessCriterion>, UlwLoopError> {
    if input.is_empty() {
        let mut details = BTreeMap::new();
        details.insert("count".into(), serde_json::json!(0));
        return Err(UlwLoopError::with_details(
            "Invalid successCriteria: provide at least one criterion or omit the field for placeholders.",
            "ULW_LOOP_ARGUMENT_INVALID",
            details,
        ));
    }

    let mut criteria = Vec::new();
    for (index, entry) in input.iter().enumerate() {
        let Some(obj) = entry.as_object() else {
            let mut details = BTreeMap::new();
            details.insert("index".into(), serde_json::json!(index));
            return Err(UlwLoopError::with_details(
                format!("Invalid successCriteria: entry {} must be an object.", index + 1),
                "ULW_LOOP_ARGUMENT_INVALID",
                details,
            ));
        };

        let user_model = obj
            .get("userModel")
            .and_then(|v| v.as_str())
            .unwrap_or("happy");
        if !is_user_model(user_model) {
            let mut details = BTreeMap::new();
            details.insert("index".into(), serde_json::json!(index));
            details.insert("userModel".into(), serde_json::json!(user_model));
            return Err(UlwLoopError::with_details(
                format!(
                    "Invalid successCriteria: entry {} userModel must be one of {}.",
                    index + 1,
                    ULW_LOOP_SUCCESS_CRITERION_USER_MODELS.join(", ")
                ),
                "ULW_LOOP_ARGUMENT_INVALID",
                details,
            ));
        }

        let essential = match obj.get("essential") {
            Some(v) => match v.as_bool() {
                Some(b) => Some(b),
                None => {
                    let mut details = BTreeMap::new();
                    details.insert("index".into(), serde_json::json!(index));
                    return Err(UlwLoopError::with_details(
                        format!("Invalid successCriteria: entry {} essential must be a boolean.", index + 1),
                        "ULW_LOOP_ARGUMENT_INVALID",
                        details,
                    ));
                }
            },
            None => Some(true),
        };

        let scenario = obj
            .get("scenario")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let scenario = match scenario {
            Some(s) => s.to_string(),
            None => {
                let mut details = BTreeMap::new();
                details.insert("index".into(), serde_json::json!(index));
                details.insert("field".into(), serde_json::json!("scenario"));
                return Err(UlwLoopError::with_details(
                    format!("Invalid successCriteria: entry {} needs a non-empty scenario.", index + 1),
                    "ULW_LOOP_ARGUMENT_INVALID",
                    details,
                ));
            }
        };

        let expected_evidence = obj
            .get("expectedEvidence")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let expected_evidence = match expected_evidence {
            Some(s) => s.to_string(),
            None => {
                let mut details = BTreeMap::new();
                details.insert("index".into(), serde_json::json!(index));
                details.insert("field".into(), serde_json::json!("expectedEvidence"));
                return Err(UlwLoopError::with_details(
                    format!("Invalid successCriteria: entry {} needs a non-empty expectedEvidence.", index + 1),
                    "ULW_LOOP_ARGUMENT_INVALID",
                    details,
                ));
            }
        };

        criteria.push(UlwLoopSuccessCriterion {
            id: criterion_id(index),
            scenario,
            user_model: user_model.to_string(),
            expected_evidence,
            essential,
            captured_evidence: None,
            status: "pending".to_string(),
            captured_at: None,
            notes: None,
            artifacts: None,
        });
    }

    Ok(criteria)
}
