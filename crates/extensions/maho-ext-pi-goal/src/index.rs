use std::path::Path;
use sha2::{Digest,Sha256};
use crate::goal::types::GoalStoreRef;

pub fn goal_store_ref(agent_dir:&Path,session_dir:Option<&Path>,cwd:&str,thread_id:&str)->GoalStoreRef{
    let base_dir=match session_dir{
        Some(directory)=>directory.join("extensions").join("pi-goal"),
        None=>agent_dir.join("extensions").join("pi-goal").join("no-session").join(cwd_store_key(cwd)),
    };
    GoalStoreRef{base_dir,thread_id:thread_id.into()}
}
pub fn cwd_store_key(cwd:&str)->String{format!("{:x}",Sha256::digest(cwd.as_bytes()))[..24].into()}

pub fn register_goal_extension(api:&mut maho_ext_api::ExtensionApi,resolve:crate::goal::lifecycle::GoalStoreResolver,send:crate::goal::lifecycle::GoalContinuationSender)->Result<(),maho_ext_api::ExtensionFailure>{
    let state=crate::goal::lifecycle::register_goal_lifecycle(api,std::sync::Arc::clone(&resolve),std::sync::Arc::clone(&send));
    let dependencies=std::sync::Arc::new(crate::goal::lifecycle::RegisteredGoalLifecycle{state,resolve,send});
    crate::goal::tool_registration::register_goal_tools(api,dependencies.clone())?;
    crate::goal::command_registration::register_goal_command(api,dependencies);
    Ok(())
}
