use std::path::Path;
use maho_ext_pi_goal::index::*;
#[test]fn session_directory_selects_store_without_agent_or_cwd(){let store=goal_store_ref(Path::new("/agent"),Some(Path::new("/sessions")),"/workspace","thread");assert_eq!(store.base_dir,Path::new("/sessions/extensions/pi-goal"));assert_eq!(store.thread_id,"thread");}
#[test]fn no_session_store_is_cwd_scoped_and_preserves_thread(){let store=goal_store_ref(Path::new("/agent"),None,"/workspace","thread");assert_eq!(store.base_dir,Path::new("/agent/extensions/pi-goal/no-session").join(cwd_store_key("/workspace")));assert_eq!(store.thread_id,"thread");assert_eq!(cwd_store_key("/workspace").len(),24);assert_ne!(cwd_store_key("/workspace"),cwd_store_key("/workspace/"));}
