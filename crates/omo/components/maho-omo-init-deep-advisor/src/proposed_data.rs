use serde::{Serialize,Deserialize};
use crate::state::SuggestedMode;
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CoverageData { pub missing_ratio:f64,pub candidate_dirs:usize,pub covered_dirs:usize }
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct DriftData { pub commits_since:u64,pub touched_ratio:f64,pub churn_loc_ratio:f64,pub days_since:f64 }
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(tag="trigger")]
pub enum EligibilityResult {
    #[serde(rename="coverage-gap")]
    CoverageGap { coverage:CoverageData },
    #[serde(rename="commit-and-touch")]
    CommitAndTouch { drift:DriftData },
    #[serde(rename="loc-churn")]
    LocChurn { drift:DriftData },
    #[serde(rename="snapshot-age")]
    SnapshotAge { drift:DriftData },
    #[serde(rename="snapshot-invalid")]
    SnapshotInvalid { drift:StaleData },
}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct StaleData { pub stale:bool }
pub fn build_proposed_data(repo:&str,eligibility:&EligibilityResult,suggested_mode:SuggestedMode) -> serde_json::Result<serde_json::Value> {
    let mut value=serde_json::to_value(eligibility)?;
    value["repo"]=serde_json::Value::String(repo.into());
    value["suggestedMode"]=serde_json::to_value(suggested_mode)?;
    match eligibility { EligibilityResult::CoverageGap {..}=>value["drift"]=serde_json::Value::Null,EligibilityResult::CommitAndTouch {..}|EligibilityResult::LocChurn {..}|EligibilityResult::SnapshotAge {..}|EligibilityResult::SnapshotInvalid {..}=>value["coverage"]=serde_json::Value::Null }
    Ok(value)
}
