use sha2::{Digest,Sha256};
use maho_core::{config::get_agent_dir,session_manager::SessionManager};
use crate::types::GoalStoreRef;
pub fn context_goal_store_ref(context:&maho_ext_api::ExtensionContext)->GoalStoreRef {
    let base_dir=if context.session_manager.session_file().is_none() {
        let hash=format!("{:x}",Sha256::digest(context.cwd.to_string_lossy().as_bytes()));
        context.agent_dir.join("extensions/goal/no-session").join(&hash[..24])
    } else {
        context.session_manager.get_session_dir().unwrap_or_else(||panic!("persisted goal context requires the session directory")).join("extensions/goal")
    };
    GoalStoreRef { base_dir,thread_id:context.session_manager.session_id().into() }
}
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
    #[test] fn persisted_session_uses_session_directory_not_cwd_namespace() {
        let temp=tempfile::tempdir().unwrap(); let directory=temp.path().to_str().unwrap();
        let session=SessionManager::create("/workspace",Some(directory),None);
        assert!(session.session_file().is_some()); let result=goal_store_ref(&session,"/different");
        assert_eq!(result.base_dir,temp.path().join("extensions/goal")); assert_eq!(result.thread_id,session.session_id());
    }
}
