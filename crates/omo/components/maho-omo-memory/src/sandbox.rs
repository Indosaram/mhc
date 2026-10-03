use std::{collections::BTreeMap,path::{Path,PathBuf}};
pub use super::sandbox_contracts::{SandboxError,SandboxPolicy,SandboxTransform};
use super::sandbox_platform::{PathSandboxInput,build_path_sandbox_transform};
pub struct ReflectionSandboxInput<'a>{pub policy:SandboxPolicy,pub worktree_dir:&'a Path,pub git_common_dir:&'a Path,pub payload_paths:&'a [PathBuf],pub runtime_writes:&'a [PathBuf],pub foreign_roots:&'a [PathBuf],pub command:&'a str,pub env:&'a BTreeMap<String,String>,pub platform:&'a str}
pub fn build_sandbox_transform(input:&ReflectionSandboxInput<'_>,which:&dyn Fn(&str)->Option<String>)->Result<SandboxTransform,SandboxError>{let writable=[vec![input.worktree_dir.into(),input.git_common_dir.into()],input.runtime_writes.to_vec()].concat();build_path_sandbox_transform(&PathSandboxInput{surface:"reflection",policy:input.policy,platform:input.platform,writable_dirs:&writable,lock_paths:&[],payload_paths:input.payload_paths,fallback_dir:input.worktree_dir,foreign_roots:input.foreign_roots,command:input.command,env:input.env},which)}
pub struct FactsSandboxInput<'a>{pub policy:SandboxPolicy,pub run_dir:&'a Path,pub payload:&'a Path,pub agent_dir:&'a Path,pub foreign_roots:&'a [PathBuf],pub command:&'a str,pub env:&'a BTreeMap<String,String>,pub platform:&'a str}
pub fn build_facts_sandbox_transform(input:&FactsSandboxInput<'_>,which:&dyn Fn(&str)->Option<String>)->Result<SandboxTransform,SandboxError>{build_path_sandbox_transform(&PathSandboxInput{surface:"facts",policy:input.policy,platform:input.platform,writable_dirs:&[input.run_dir.into()],lock_paths:&[input.agent_dir.join("settings.json.lock"),input.agent_dir.join("auth.json.lock")],payload_paths:&[input.payload.into()],fallback_dir:input.run_dir,foreign_roots:input.foreign_roots,command:input.command,env:input.env},which)}
pub struct FactsSpawnSandboxInput<'a>{pub policy:SandboxPolicy,pub agent_dir:&'a Path,pub foreign_roots:&'a [PathBuf],pub platform:&'a str}
pub fn apply_facts_sandbox(
    mut args:crate::worker::spawn_types::FactsSpawnArgs,input:FactsSpawnSandboxInput<'_>,which:&dyn Fn(&str)->Option<String>,
    warn:impl FnOnce(&str,&crate::worker::spawn_types::FactsSpawnArgs),
)->Result<crate::worker::spawn_types::FactsSpawnArgs,SandboxError>{
    let FactsSpawnSandboxInput{policy,agent_dir,foreign_roots,platform}=input;
    let transform=build_facts_sandbox_transform(&FactsSandboxInput{policy,run_dir:&args.paths.run_dir,payload:&args.paths.payload,agent_dir,foreign_roots,command:&args.command,env:&args.env,platform},which)?;
    if let Some(warning)=&transform.warning{warn(warning,&args);}
    let spawn=transform.apply(crate::sandbox_contracts::SandboxSpawnArgs{command:args.command,args:args.args,cwd:args.cwd,env:args.env});
    args.command=spawn.command;args.args=spawn.args;args.cwd=spawn.cwd;args.env=spawn.env;Ok(args)
}
#[cfg(test)]mod tests{
    use super::*;
    fn facts_args(root:&Path)->crate::worker::spawn_types::FactsSpawnArgs{
        crate::worker::spawn_types::FactsSpawnArgs{run_id:"run".into(),attempt:2,hard_deadline_at:42.0,model:"p/m".into(),thinking:Some("low".into()),next_attempt:None,command:"/bin/sh".into(),args:vec!["-c".into(),"exit 0".into()],cwd:root.into(),env:BTreeMap::new(),paths:crate::worker::spawn_types::FactsSpawnPaths{run_dir:root.into(),payload:root.join("payload.json"),extraction:root.join("extraction.jsonl")}}
    }
    #[test]fn facts_spawn_auto_warns_with_original_metadata(){
        let root=tempfile::tempdir().unwrap();let mut warned=false;
        let args=apply_facts_sandbox(facts_args(root.path()),FactsSpawnSandboxInput{policy:SandboxPolicy::Auto,agent_dir:root.path(),foreign_roots:&[],platform:"linux"},&|_|Some("bwrap".into()),|_,args|{warned=true;assert_eq!(args.run_id,"run");assert_eq!(args.attempt,2);assert_eq!(args.model,"p/m");assert_eq!(args.command,"/bin/sh");}).unwrap();
        assert!(warned);assert_eq!(args.hard_deadline_at,42.0);assert_eq!(args.command,"/bin/sh");assert_eq!(args.paths.extraction,root.path().join("extraction.jsonl"));
    }
    #[test]fn facts_spawn_darwin_transforms_only_execution_fields(){
        let root=tempfile::tempdir().unwrap();std::fs::write(root.path().join("payload.json"),"{}").unwrap();
        let args=apply_facts_sandbox(facts_args(root.path()),FactsSpawnSandboxInput{policy:SandboxPolicy::Required,agent_dir:root.path(),foreign_roots:&[],platform:"darwin"},&|_|Some("sandbox-exec".into()),|_,_|panic!("unexpected degradation")).unwrap();
        assert_eq!(args.command,"sandbox-exec");assert_eq!(&args.args[args.args.len()-4..],["--","/bin/sh","-c","exit 0"]);assert_eq!(args.run_id,"run");assert_eq!(args.attempt,2);assert_eq!(args.thinking.as_deref(),Some("low"));assert_eq!(args.hard_deadline_at,42.0);assert!(args.env.contains_key("TMPDIR"));assert_eq!(args.paths.payload,root.path().join("payload.json"));
    }
    #[test]fn reflection_wraps_both_repo_and_common_directory(){let root=tempfile::tempdir().unwrap();let work=root.path().join("work");let git=root.path().join("git");for path in [&work,&git]{std::fs::create_dir(path).unwrap();}let env=BTreeMap::new();let transform=build_sandbox_transform(&ReflectionSandboxInput{policy:SandboxPolicy::Required,worktree_dir:&work,git_common_dir:&git,payload_paths:&[],runtime_writes:&[],foreign_roots:&[],command:"/bin/sh",env:&env,platform:"linux"},&|_|Some("bwrap".into())).unwrap();let args=transform.apply(super::super::sandbox_contracts::SandboxSpawnArgs{command:"/bin/sh".into(),args:vec![],cwd:work.clone(),env});assert_eq!(args.args.iter().filter(|arg|arg.as_str()=="--bind").count(),2);assert!(args.args.contains(&git.to_string_lossy().into_owned()));}
    #[test]fn facts_linux_required_fails_auto_warns(){let root=tempfile::tempdir().unwrap();let env=BTreeMap::new();let mut input=FactsSandboxInput{policy:SandboxPolicy::Required,run_dir:root.path(),payload:&root.path().join("payload"),agent_dir:root.path(),foreign_roots:&[],command:"/bin/sh",env:&env,platform:"linux"};assert!(matches!(build_facts_sandbox_transform(&input,&|_|Some("bwrap".into())),Err(SandboxError::Unavailable{..})));input.policy=SandboxPolicy::Auto;let transform=build_facts_sandbox_transform(&input,&|_|Some("bwrap".into())).unwrap();assert!(!transform.was_sandboxed);assert!(transform.warning.unwrap().starts_with("facts sandbox unavailable"));}
}
