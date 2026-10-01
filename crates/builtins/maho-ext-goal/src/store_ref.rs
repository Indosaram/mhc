use sha2::{Digest,Sha256};
use maho_core::{config::get_agent_dir,session_manager::SessionManager};
use crate::types::GoalStoreRef;
pub fn goal_store_ref(session:&SessionManager,cwd:&str)->GoalStoreRef {
    let base_dir=if session.session_file().is_none() {
        let hash=format!("{:x}",Sha256::digest(cwd.as_bytes()));
        std::path::PathBuf::from(get_agent_dir()).join("extensions/goal/no-session").join(&hash[..24])
    } else { std::path::PathBuf::from(session.session_dir()).join("extensions/goal") };
    GoalStoreRef { base_dir,thread_id:session.session_id().into() }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn no_session_namespace_uses_cwd_hash_and_thread_id() {
        let session=SessionManager::in_memory("/workspace",None,None); let result=goal_store_ref(&session,"/workspace"); let hash=format!("{:x}",Sha256::digest(b"/workspace"));
        assert_eq!(result.base_dir.file_name().unwrap(),&hash[..24]); assert_eq!(result.thread_id,session.session_id()); assert!(result.base_dir.to_string_lossy().contains("extensions/goal/no-session"));
    }
    #[test] fn different_working_directories_have_separate_namespaces() {
        let session=SessionManager::in_memory("/workspace",None,None); let first=goal_store_ref(&session,"/first"); let second=goal_store_ref(&session,"/second"); assert_ne!(first.base_dir,second.base_dir);
    }
}
