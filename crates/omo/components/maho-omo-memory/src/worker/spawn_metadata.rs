use memory_core::reflection::worktree::ReflectionWorktree;
use super::spawn_types::ReflectionSpawnArgs;
pub struct RunMetadata<'a>{pub run_id:&'a str,pub kind:&'a str,pub trigger:&'a str,pub origin:Option<&'a str>,pub merge_policy:&'a str,pub target_doc:Option<&'a str>,pub worktree:&'a ReflectionWorktree}
#[derive(Debug,PartialEq,Eq)]pub enum SpawnMetadataError{Required,DreamTriggerOrigin,DreamTarget}
pub fn require_run_metadata(args:&ReflectionSpawnArgs)->Result<RunMetadata<'_>,SpawnMetadataError>{
    let (Some(run_id),Some(kind),Some(trigger),Some(merge_policy),Some(worktree))=(&args.run_id,&args.kind,&args.trigger,&args.merge_policy,&args.worktree)else{return Err(SpawnMetadataError::Required);};
    if (kind=="dream")!=(trigger=="dream")||(kind=="dream"&&args.origin.is_none()){return Err(SpawnMetadataError::DreamTriggerOrigin);}
    if args.target_doc.is_some()&&kind!="dream"{return Err(SpawnMetadataError::DreamTarget);}
    Ok(RunMetadata{run_id,kind,trigger,origin:args.origin.as_deref(),merge_policy,target_doc:args.target_doc.as_deref(),worktree})
}
#[cfg(test)]mod tests{
    use super::*;
    use super::super::spawn_types::ReflectionSpawnPaths;
    fn args(root:&std::path::Path)->ReflectionSpawnArgs{
        let paths=memory_core::identity::layout::build_identity_paths(root,"agent");let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();
        let worktree=ReflectionWorktree{parent:engine.repo,dir:root.join("w"),branch:"memory/run".into(),base_commit_sha:"sha".into(),git_file_path:root.join("w/.git"),git_file_snapshot:"gitdir: missing".into(),common_config_path:root.join("config"),common_config_snapshot:None,exec:memory_core::git::exec::create_git_exec(Default::default())};
        ReflectionSpawnArgs{parent_session_file:None,run_id:Some("run".into()),attempt:1,hard_deadline_at:0.0,category:"quick".into(),conversation_ids:vec![],model:"p/m".into(),thinking:None,next_attempt:None,kind:Some("reflection".into()),trigger:Some("manual".into()),origin:None,merge_policy:Some("auto".into()),target_doc:None,worktree:Some(worktree),command:"maho".into(),args:vec![],cwd:root.into(),env:Default::default(),paths:ReflectionSpawnPaths{session_dir:root.into(),worktree:root.into(),git_common_dir:root.into(),transcript:root.join("transcript"),persona:root.join("persona"),prompt:root.join("prompt"),skills_usage:None,dream_state:None,dream_policy:None,dream_target:None}}
    }
    #[test]fn required_fields_reject_without_metadata(){let root=tempfile::tempdir().unwrap();let mut args=args(root.path());assert_eq!(require_run_metadata(&args).unwrap().run_id,"run");args.run_id=None;assert!(matches!(require_run_metadata(&args),Err(SpawnMetadataError::Required)));}
    #[test]fn dream_requires_trigger_and_origin(){let root=tempfile::tempdir().unwrap();let mut args=args(root.path());args.kind=Some("dream".into());assert!(matches!(require_run_metadata(&args),Err(SpawnMetadataError::DreamTriggerOrigin)));args.trigger=Some("dream".into());assert!(matches!(require_run_metadata(&args),Err(SpawnMetadataError::DreamTriggerOrigin)));args.origin=Some("idle".into());assert_eq!(require_run_metadata(&args).unwrap().origin,Some("idle"));}
    #[test]fn target_requires_dream_and_worktree_reference_is_preserved(){let root=tempfile::tempdir().unwrap();let mut args=args(root.path());args.target_doc=Some("reference/doc.md".into());assert!(matches!(require_run_metadata(&args),Err(SpawnMetadataError::DreamTarget)));args.kind=Some("dream".into());args.trigger=Some("dream".into());args.origin=Some("manual".into());let metadata=require_run_metadata(&args).unwrap();assert_eq!(metadata.target_doc,Some("reference/doc.md"));assert!(std::ptr::eq(metadata.worktree,args.worktree.as_ref().unwrap()));}
}
