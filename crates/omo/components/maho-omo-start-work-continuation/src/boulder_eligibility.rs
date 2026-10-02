use std::path::{Path, PathBuf};
use boulder_state::{BoulderWorkState, BoulderWorkStatus, BoulderStateError, PlanChecklist, get_work_for_session, resolve_boulder_plan_path_for_work, get_plan_checklist};

pub struct ContinuableWork { pub work: BoulderWorkState, pub plan_path: PathBuf, pub checklist: PlanChecklist }
pub fn find_continuable_boulder_work(cwd: &Path, session_id: &str) -> Result<Option<ContinuableWork>, BoulderStateError> {
    let work = match get_work_for_session(cwd, &format!("senpi:{session_id}")) {
        Ok(work) => work,
        Err(BoulderStateError::Read { .. } | BoulderStateError::InvalidJson { .. } | BoulderStateError::InvalidShape { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(work) = work else { return Ok(None); };
    if !matches!(work.status(),Some(BoulderWorkStatus::Active | BoulderWorkStatus::Paused)) { return Ok(None); }
    let plan_path=resolve_boulder_plan_path_for_work(cwd,&work);
    let checklist=get_plan_checklist(&plan_path);
    if checklist.total==0 { return Ok(None); }
    Ok(Some(ContinuableWork{work,plan_path,checklist}))
}
