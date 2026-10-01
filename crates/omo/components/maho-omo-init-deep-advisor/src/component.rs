use std::{path::Path,sync::OnceLock};
use crate::{git_helpers::{git_is_repo,run},runtime::AdvisorPreflight};
static PROCESS_START_TIME:OnceLock<f64>=OnceLock::new();
pub fn process_start_time()->f64 {*PROCESS_START_TIME.get_or_init(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()*1000.0)}
pub fn advisor_preflight(has_ui:bool,disabled:bool,cwd:&Path,onboarding_state_dir:&Path,marker_mtime:Option<f64>,process_start:f64)->std::io::Result<Option<AdvisorPreflight>> {
    if !has_ui || disabled || !git_is_repo(cwd) {return Ok(None);}
    let root=run(cwd,&["rev-parse","--show-toplevel"])?;
    if marker_mtime.is_none_or(|mtime|mtime>=process_start) {return Ok(None);}
    Ok(Some(AdvisorPreflight {root:root.trim().into(),state_dir:onboarding_state_dir.join("init-deep-advisor-state")}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ui_and_disabled_gates() {let (t,_)=crate::git_helpers::tests::repo();assert!(advisor_preflight(false,false,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());assert!(advisor_preflight(true,true,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());}
    #[test] fn onboarding_must_predate_process() {let (t,_)=crate::git_helpers::tests::repo();for marker in [None,Some(2.0),Some(3.0)] {assert!(advisor_preflight(true,false,t.path(),t.path(),marker,2.0).unwrap().is_none());}let p=advisor_preflight(true,false,t.path(),t.path(),Some(1.0),2.0).unwrap().unwrap();assert_eq!(p.root,t.path());assert_eq!(p.state_dir,t.path().join("init-deep-advisor-state"));}
    #[test] fn nonrepo_skips() {let t=tempfile::tempdir().unwrap();assert!(advisor_preflight(true,false,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());}
}
