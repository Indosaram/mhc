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
