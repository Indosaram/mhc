use std::{collections::BTreeMap,path::Path};
use memory_core::facts::{FactsPayload,serialize_facts_payload,load_facts_persona};
use super::{run_artifacts::RunAttempt,spawn_types::{FactsSpawnArgs,FactsSpawnPaths},model_preflight::Launcher};
pub struct PrepareFactsSpawnInput<'a>{pub run_id:&'a str,pub run_dir:&'a Path,pub payload:&'a FactsPayload,pub model:&'a str,pub thinking:Option<&'a str>,pub attempt:Option<u32>,pub hard_deadline_at:Option<f64>,pub next_attempt:Option<RunAttempt>,pub env:BTreeMap<String,String>,pub launch:Launcher,pub now_ms:f64}
pub fn prepare_facts_spawn(input:PrepareFactsSpawnInput<'_>)->Result<FactsSpawnArgs,std::io::Error>{
    let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
    #[cfg(unix)]{use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
    builder.create(input.run_dir)?;
    let payload=input.run_dir.join("facts-payload.json");let extraction=input.run_dir.join("extraction.jsonl");
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;match std::fs::set_permissions(&payload,std::fs::Permissions::from_mode(0o600)){Ok(())=>{},Err(e)if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e)}}
    std::fs::write(&payload,serialize_facts_payload(input.payload))?;
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&payload,std::fs::Permissions::from_mode(0o400))?;}
    let mut env=input.env;env.insert("FACTS_PAYLOAD_PATH".into(),payload.to_string_lossy().into_owned());env.insert("FACTS_EXTRACTION_PATH".into(),extraction.to_string_lossy().into_owned());env.insert("SENPI_MEMORY_FACTS".into(),"1".into());env.insert("SENPI_PTY_FORCE_PIPE".into(),"1".into());
    let mut args=input.launch.prefix_args;args.extend(["-p".into(),"--system-prompt".into(),load_facts_persona().into(),"--tools".into(),"read,write".into(),"--no-extensions".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--no-context-files".into(),"--session-dir".into(),input.run_dir.to_string_lossy().into_owned(),"--model".into(),input.model.into()]);
    if let Some(thinking)=input.thinking{args.extend(["--thinking".into(),thinking.into()]);}
    args.push(format!("Read {} and write only {} according to the system prompt.",payload.display(),extraction.display()));
    Ok(FactsSpawnArgs{run_id:input.run_id.into(),attempt:input.attempt.unwrap_or(1),hard_deadline_at:input.hard_deadline_at.unwrap_or(input.now_ms+900_000.0),model:input.model.into(),thinking:input.thinking.map(str::to_owned),next_attempt:input.next_attempt,command:input.launch.command,args,cwd:input.run_dir.into(),env,paths:FactsSpawnPaths{run_dir:input.run_dir.into(),payload,extraction}})
}
#[cfg(test)]mod tests{
    use super::*;
    fn payload()->FactsPayload{FactsPayload{version:1,identity:"agent".into(),today:"2026-10-02".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]},entries:vec![]}}
    #[test]fn shared_serializer_and_launch_flags(){let root=tempfile::tempdir().unwrap();let payload=payload();let args=prepare_facts_spawn(PrepareFactsSpawnInput{run_id:"facts",run_dir:root.path(),payload:&payload,model:"p/m",thinking:Some("low"),attempt:None,hard_deadline_at:None,next_attempt:None,env:BTreeMap::from([("KEEP".into(),"yes".into())]),launch:Launcher{command:"maho".into(),prefix_args:vec!["prefix".into()]},now_ms:42.0}).unwrap();assert_eq!(std::fs::read_to_string(&args.paths.payload).unwrap(),serialize_facts_payload(&payload));assert_eq!(args.args[0],"prefix");assert_eq!(args.attempt,1);assert_eq!(args.hard_deadline_at,900042.0);assert_eq!(args.env["KEEP"],"yes");assert_eq!(args.env["SENPI_PTY_FORCE_PIPE"],"1");assert!(args.args.windows(2).any(|a|a==["--tools","read,write"]));assert!(!args.paths.extraction.exists());}
    #[test]fn retry_rewrites_readonly_payload(){let root=tempfile::tempdir().unwrap();let mut payload=payload();for id in ["first","second"]{payload.identity=id.into();let args=prepare_facts_spawn(PrepareFactsSpawnInput{run_id:"facts",run_dir:root.path(),payload:&payload,model:"p/m",thinking:None,attempt:Some(2),hard_deadline_at:Some(123.0),next_attempt:None,env:Default::default(),launch:Launcher{command:"maho".into(),prefix_args:vec![]},now_ms:0.0}).unwrap();assert_eq!(std::fs::read_to_string(&args.paths.payload).unwrap(),serialize_facts_payload(&payload));assert_eq!(args.hard_deadline_at,123.0);
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&args.paths.payload).unwrap().permissions().mode()&0o777,0o400);}
    }}
}
