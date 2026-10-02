use std::{io,path::Path};
use crate::{constants::*,coverage::{compute_coverage_ratio,should_propose_init},drift::{compute_drift,should_propose_refresh,DriftMetrics},proposed_data::{CoverageData,DriftData,EligibilityResult,StaleData},state::{read_snapshot,SnapshotReadResult}};
pub fn compute_eligibility(root:&Path,current_head:&str,last_head:Option<&str>,cooldown_until:f64,now:f64) -> io::Result<Option<EligibilityResult>> {
    let drift=match read_snapshot(root) {
        SnapshotReadResult::Missing=> {
            let coverage=compute_coverage_ratio(root)?;
            if !should_propose_init(coverage.as_ref().map(|c|c.missing_ratio),root.join("AGENTS.md").exists()) || Some(current_head)==last_head || now<cooldown_until { return Ok(None); }
            return Ok(coverage.map(|c|EligibilityResult::CoverageGap { coverage:CoverageData { missing_ratio:c.missing_ratio,candidate_dirs:c.candidate_dirs,covered_dirs:c.covered_dirs } }));
        },
        SnapshotReadResult::Invalid=>DriftMetrics::Stale,
        SnapshotReadResult::Valid(snapshot)=>compute_drift(root,&snapshot,now)?,
    };
    if !should_propose_refresh(&drift,current_head,last_head,cooldown_until,now) { return Ok(None); }
    let result=match drift {
        DriftMetrics::Stale=>EligibilityResult::SnapshotInvalid { drift:StaleData {stale:true} },
        DriftMetrics::Valid(d)=> {
            let data=DriftData { commits_since:d.commits_since,touched_ratio:d.touched_ratio,churn_loc_ratio:d.churn_loc_ratio,days_since:d.days_since };
            if d.commits_since>=COMMIT_DISTANCE_THRESHOLD && d.touched_ratio>=TOUCHED_RATIO_THRESHOLD { EligibilityResult::CommitAndTouch {drift:data} }
            else if d.churn_loc_ratio>=CHURN_LOC_RATIO_THRESHOLD { EligibilityResult::LocChurn {drift:data} }
            else if d.days_since>=DAYS_SINCE_THRESHOLD { EligibilityResult::SnapshotAge {drift:data} }
            else { return Err(io::Error::other("eligible drift has no trigger")); }
        }
    };
    Ok(Some(result))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test] fn missing_coverage_proposes() { let t=tempfile::tempdir().unwrap(); fs::create_dir(t.path().join("src")).unwrap(); for n in 0..8 { fs::write(t.path().join("src").join(format!("{n}.ts")),"x").unwrap(); } assert!(matches!(compute_eligibility(t.path(),"head",None,0.0,1.0).unwrap(),Some(EligibilityResult::CoverageGap {..}))); assert!(compute_eligibility(t.path(),"head",Some("head"),0.0,1.0).unwrap().is_none()); assert!(compute_eligibility(t.path(),"head",None,2.0,1.0).unwrap().is_none()); }
    #[test] fn invalid_snapshot_proposes() { let t=tempfile::tempdir().unwrap(); fs::create_dir(t.path().join(".omo")).unwrap(); fs::write(t.path().join(".omo/init-deep.json"),"{bad").unwrap(); assert!(matches!(compute_eligibility(t.path(),"head",None,0.0,1.0).unwrap(),Some(EligibilityResult::SnapshotInvalid {..}))); assert!(compute_eligibility(t.path(),"head",None,2.0,1.0).unwrap().is_none()); }
    #[test] fn proposal_shape() { let e=EligibilityResult::SnapshotInvalid {drift:StaleData {stale:true}}; let v=crate::proposed_data::build_proposed_data("repo",&e,crate::state::SuggestedMode::Local).unwrap(); assert_eq!(v["trigger"],"snapshot-invalid"); assert_eq!(v["suggestedMode"],"local"); assert!(v["coverage"].is_null()); assert_eq!(v["drift"]["stale"],true); }
}
