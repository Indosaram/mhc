use maho_ext_api::{ExtensionApi,ExtensionFailure};
use crate::{types::{LoopState,CronEntry,LoopPhase},tools::SCHEDULE_WAKEUP_TOOL};
pub fn sync_schedule_wakeup_activation(api:&ExtensionApi,state:&LoopState)->Result<(),ExtensionFailure> {
    let wanted=state.entries.values().any(|entry|matches!(entry,CronEntry::Dynamic { lifecycle,.. } if !matches!(lifecycle.phase,LoopPhase::Ended|LoopPhase::Suspended)));
    let mut active=api.get_active_tools()?;
    let present=active.iter().any(|name|name==SCHEDULE_WAKEUP_TOOL);
    if wanted==present { return Ok(()); }
    if wanted { active.push(SCHEDULE_WAKEUP_TOOL.into()); } else { active.retain(|name|name!=SCHEDULE_WAKEUP_TOOL); }
    api.set_active_tools(active)
}
