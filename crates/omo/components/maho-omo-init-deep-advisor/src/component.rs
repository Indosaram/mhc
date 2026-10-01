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
pub type OnboardingMarkerMtime=std::sync::Arc<dyn Fn(&Path)->Option<f64>+Send+Sync>;
pub struct InitDeepAdvisorComponent {pub state_dir:std::path::PathBuf,pub skills_root:std::path::PathBuf,pub marker_mtime:OnboardingMarkerMtime,pub process_start:f64}
impl maho_ext_api::Extension for InitDeepAdvisorComponent {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        use maho_ext_api::{EventKind,EventResult,ExtensionEvent,FlagValue,SessionReason};
        let state_dir=self.state_dir.clone();let skills=self.skills_root.clone();let marker=std::sync::Arc::clone(&self.marker_mtime);let start=self.process_start;
        let runtime=api.runtime.clone();let profile=api.profile.clone();let events=api.events.clone();let registered=api.registered.clone();
        api.on(EventKind::SessionStart,std::sync::Arc::new(move |event,ctx| {
            if let ExtensionEvent::SessionStart(event)=event && event.reason==SessionReason::Startup {
                let preflight=advisor_preflight(ctx.has_ui,runtime.get_flag("omo-senpi-init-deep-advisor-disabled")==Some(FlagValue::Boolean(true)),&ctx.cwd,&state_dir,marker(&state_dir),start);
                match preflight {Ok(Some(preflight))=>{let mut api=maho_ext_api::ExtensionApi::new(registered.clone(),profile.clone(),events.clone(),runtime.clone());api.cwd=ctx.cwd.clone();let ctx=ctx.clone();let skills=skills.clone();tokio::spawn(async move {let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()*1000.0;if let Err(error)=crate::runtime::run_advisor_after_preflight(&api,&ctx,&preflight,&skills,now).await {eprintln!("init-deep-advisor failed: {error}");}});},Ok(None)=>{},Err(error)=>eprintln!("init-deep-advisor failed: {error}")}
            }
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ui_and_disabled_gates() {let (t,_)=crate::git_helpers::tests::repo();assert!(advisor_preflight(false,false,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());assert!(advisor_preflight(true,true,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());}
    #[test] fn onboarding_must_predate_process() {let (t,_)=crate::git_helpers::tests::repo();for marker in [None,Some(2.0),Some(3.0)] {assert!(advisor_preflight(true,false,t.path(),t.path(),marker,2.0).unwrap().is_none());}let p=advisor_preflight(true,false,t.path(),t.path(),Some(1.0),2.0).unwrap().unwrap();assert_eq!(p.root,t.path());assert_eq!(p.state_dir,t.path().join("init-deep-advisor-state"));}
    #[test] fn nonrepo_skips() {let t=tempfile::tempdir().unwrap();assert!(advisor_preflight(true,false,t.path(),t.path(),Some(1.0),2.0).unwrap().is_none());}
}
