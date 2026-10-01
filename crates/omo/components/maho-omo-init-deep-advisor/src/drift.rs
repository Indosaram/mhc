use std::{io,path::Path};
use crate::{constants::*,git_helpers::*,state::InitDeepSnapshotV1};
#[derive(Debug,Clone)]
pub struct ValidDrift { pub commits_since:u64,pub touched_files:usize,pub tracked_files:usize,pub touched_ratio:f64,pub churn_loc:u64,pub total_loc:usize,pub churn_loc_ratio:f64,pub days_since:f64 }
#[derive(Debug,Clone)]
pub enum DriftMetrics { Valid(ValidDrift),Stale }
fn number(n:usize) -> f64 { (0..n).fold(0.0,|v,_|v+1.0) }
pub fn compute_drift(root:&Path,snapshot:&InitDeepSnapshotV1,now:f64) -> io::Result<DriftMetrics> {
    if git_object_type(root,&snapshot.commit_sha).as_deref()!=Some("commit") { return Ok(DriftMetrics::Stale); }
    let commits_since=git_commits_since(root,&snapshot.commit_sha)?;
    let touched_files=git_touched_files_since(root,&snapshot.commit_sha)?.len();
    let tracked_files=git_tracked_file_count(root)?;
    let churn_loc=git_churn_loc(root,&snapshot.commit_sha)?;
    let total_loc=git_total_loc(root)?;
    let churn=churn_loc.to_string().parse::<f64>().map_err(io::Error::other)?;
    Ok(DriftMetrics::Valid(ValidDrift { commits_since,touched_files,tracked_files,touched_ratio:number(touched_files)/number(tracked_files.max(1)),churn_loc,total_loc,churn_loc_ratio:churn/number(total_loc.max(1)),days_since:(now-snapshot.timestamp)/86_400_000.0 }))
}
pub fn should_propose_refresh(drift:&DriftMetrics,current_head:&str,last_head:Option<&str>,cooldown_until:f64,now:f64) -> bool {
    if Some(current_head)==last_head || now<cooldown_until { return false; }
    match drift { DriftMetrics::Stale=>true,DriftMetrics::Valid(d)=>(d.commits_since>=COMMIT_DISTANCE_THRESHOLD && d.touched_ratio>=TOUCHED_RATIO_THRESHOLD) || d.churn_loc_ratio>=CHURN_LOC_RATIO_THRESHOLD || d.days_since>=DAYS_SINCE_THRESHOLD }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{git_helpers::tests::{repo,commit},state::SuggestedMode};
    use std::fs;
    fn snap(sha:String,timestamp:f64) -> InitDeepSnapshotV1 { InitDeepSnapshotV1 { commit_sha:sha,file_count:10.0,loc:1000.0,timestamp,mode:SuggestedMode::Local } }
    fn threshold() -> DriftMetrics { DriftMetrics::Valid(ValidDrift { commits_since:40,touched_files:2,tracked_files:10,touched_ratio:0.2,churn_loc:100,total_loc:1000,churn_loc_ratio:0.1,days_since:0.0 }) }
    #[test] fn forty_commits() { let (t,_)=repo(); for n in 0..10 { fs::write(t.path().join("src").join(format!("file{n}.ts")),"line\n".repeat(100)).unwrap(); } fs::remove_file(t.path().join("src/a.ts")).unwrap(); fs::remove_file(t.path().join("src/b.ts")).unwrap(); let base=commit(t.path()); for n in 0..40 { fs::write(t.path().join("src/file0.ts"),format!("{}change{n}\n","line\n".repeat(100))).unwrap(); fs::write(t.path().join("src/file1.ts"),format!("{}change{n}\n","line\n".repeat(100))).unwrap(); commit(t.path()); } let DriftMetrics::Valid(d)=compute_drift(t.path(),&snap(base,0.0),0.0).unwrap() else { panic!("valid commit"); }; assert_eq!(d.commits_since,40); assert_eq!(d.touched_files,2); assert_eq!(d.tracked_files,10); assert!((d.touched_ratio-0.2).abs()<1e-10); assert!(d.churn_loc>0); assert!(d.churn_loc_ratio<0.25); }
    #[test] fn unchanged() { let (t,sha)=repo(); let DriftMetrics::Valid(d)=compute_drift(t.path(),&snap(sha,0.0),0.0).unwrap() else { panic!("valid"); }; assert_eq!(d.commits_since,0); assert_eq!(d.touched_files,0); assert_eq!(d.churn_loc,0); }
    #[test] fn age() { let (t,sha)=repo(); let DriftMetrics::Valid(d)=compute_drift(t.path(),&snap(sha,0.0),8_640_000_000.0).unwrap() else { panic!("valid"); }; assert!((d.days_since-100.0).abs()<f64::EPSILON); }
    #[test] fn invalid_sha() { let (t,_)=repo(); assert!(matches!(compute_drift(t.path(),&snap("0".repeat(40),0.0),0.0).unwrap(),DriftMetrics::Stale)); }
    #[test] fn tag_stale() { let (t,_)=repo(); run(t.path(),&["tag","-a","v1","-m","tagged"]).unwrap(); let sha=run(t.path(),&["rev-parse","v1"]).unwrap(); assert!(matches!(compute_drift(t.path(),&snap(sha.trim().into(),0.0),0.0).unwrap(),DriftMetrics::Stale)); }
    #[test] fn tree_stale() { let (t,_)=repo(); let sha=run(t.path(),&["rev-parse","HEAD^{tree}"]).unwrap(); assert!(matches!(compute_drift(t.path(),&snap(sha.trim().into(),0.0),0.0).unwrap(),DriftMetrics::Stale)); }
    #[test] fn blob_stale() { let (t,_)=repo(); let sha=run(t.path(),&["rev-parse","HEAD:src/a.ts"]).unwrap(); assert!(matches!(compute_drift(t.path(),&snap(sha.trim().into(),0.0),0.0).unwrap(),DriftMetrics::Stale)); }
    #[test] fn composite_threshold() { assert!(should_propose_refresh(&threshold(),"head",None,0.0,1.0)); }
    #[test] fn repeated_head() { assert!(!should_propose_refresh(&threshold(),"head",Some("head"),0.0,1.0)); }
    #[test] fn cooldown() { assert!(!should_propose_refresh(&threshold(),"head",None,2.0,1.0)); }
    #[test] fn stale_proposes() { assert!(should_propose_refresh(&DriftMetrics::Stale,"head",None,0.0,1.0)); }
    #[test] fn stale_cooldown() { assert!(!should_propose_refresh(&DriftMetrics::Stale,"head",None,2.0,1.0)); }
    #[test] fn low_touch_suppresses() { let DriftMetrics::Valid(mut d)=threshold() else { unreachable!() }; d.touched_ratio=0.05; assert!(!should_propose_refresh(&DriftMetrics::Valid(d),"head",None,0.0,1.0)); }
    #[test] fn churn_threshold() { let DriftMetrics::Valid(mut d)=threshold() else { unreachable!() }; d.commits_since=1; d.touched_ratio=0.01; d.churn_loc_ratio=0.25; assert!(should_propose_refresh(&DriftMetrics::Valid(d),"head",None,0.0,1.0)); }
    #[test] fn age_threshold() { let DriftMetrics::Valid(mut d)=threshold() else { unreachable!() }; d.commits_since=0; d.touched_ratio=0.0; d.churn_loc_ratio=0.0; d.days_since=90.0; assert!(should_propose_refresh(&DriftMetrics::Valid(d),"head",None,0.0,1.0)); }
}
