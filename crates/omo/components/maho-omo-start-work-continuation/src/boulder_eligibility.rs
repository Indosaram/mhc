use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use boulder_state::{
    BoulderStateError, BoulderWorkState, BoulderWorkStatus, PlanChecklist,
    ReconcileStaleWorksOptions, get_plan_checklist, get_work_for_session, reconcile_stale_works,
    resolve_boulder_plan_path_for_work,
};
use maho_omo_agent_home::resolve_agent_home;

pub struct ContinuableWork {
    pub work: BoulderWorkState,
    pub plan_path: PathBuf,
    pub checklist: PlanChecklist,
}

fn default_sessions_directory() -> PathBuf {
    // `vars()` panics on a non-Unicode entry, so read the OsString form and drop what cannot be
    // represented; the resolver only needs the three agent-dir override names.
    let env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.to_str()?.to_string(), value.to_str()?.to_string())))
        .collect();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(PathBuf::new, PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_default();
    resolve_agent_home(&env, &home, &cwd, Path::exists).join("sessions")
}

pub fn find_continuable_boulder_work(
    cwd: &Path,
    session_id: &str,
) -> Result<Option<ContinuableWork>, BoulderStateError> {
    find_continuable_boulder_work_in(cwd, session_id, &default_sessions_directory())
}

pub fn find_continuable_boulder_work_in(
    cwd: &Path,
    session_id: &str,
    sessions_directory: &Path,
) -> Result<Option<ContinuableWork>, BoulderStateError> {
    // Repair before the read: a work whose session died abnormally is still `active` on disk, and
    // this is the first place a new session in the project looks at the record (#8413).
    let _ = reconcile_stale_works(
        cwd,
        &ReconcileStaleWorksOptions {
            sessions_directory: Some(sessions_directory.to_path_buf()),
            ..Default::default()
        },
    );

    let work = match get_work_for_session(cwd, &format!("senpi:{session_id}")) {
        Ok(work) => work,
        Err(
            BoulderStateError::Read { .. }
            | BoulderStateError::InvalidJson { .. }
            | BoulderStateError::InvalidShape { .. },
        ) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(work) = work else { return Ok(None) };
    if !matches!(
        work.status(),
        Some(BoulderWorkStatus::Active | BoulderWorkStatus::Paused)
    ) {
        return Ok(None);
    }
    let plan_path = resolve_boulder_plan_path_for_work(cwd, &work);
    let checklist = get_plan_checklist(&plan_path);
    if checklist.total == 0 {
        return Ok(None);
    }
    Ok(Some(ContinuableWork {
        work,
        plan_path,
        checklist,
    }))
}
