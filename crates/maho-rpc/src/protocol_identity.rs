use std::{collections::HashMap,sync::OnceLock};
use sha2::{Digest,Sha256};
use crate::host_protocol_info::{RpcLaunchProfile,RpcLaunchProfileCore};
pub const HOST_GENERATION_ENV:&str="SENPI_RPC_HOST_GENERATION";
pub const HOST_INSTANCE_ID_ENV:&str="SENPI_RPC_HOST_INSTANCE_ID";
static INSTANCE_ID:OnceLock<String>=OnceLock::new();
pub fn resolve_instance_id(value:Option<&str>)->String{value.filter(|value|!value.trim().is_empty()).map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string())}
pub fn host_instance_id()->&'static str{INSTANCE_ID.get_or_init(||resolve_instance_id(std::env::var(HOST_INSTANCE_ID_ENV).ok().as_deref()))}
pub fn host_generation(env:&HashMap<String,String>)->f64{env.get(HOST_GENERATION_ENV).filter(|value|!value.is_empty()&&value.bytes().all(|byte|byte.is_ascii_digit())).and_then(|value|value.parse().ok()).unwrap_or(0.)}
pub fn launch_profile_from_core(mut core:RpcLaunchProfileCore)->Result<RpcLaunchProfile,serde_json::Error>{core.extensions.sort();core.extensions.dedup();let canonical=serde_json::to_vec(&core)?;Ok(RpcLaunchProfile{profile_id:format!("{:x}",Sha256::digest(canonical)),core})}
/// Derives the launch profile from one argument vector, resolving extensions against `cwd`
/// (senpi `hostLaunchProfile`). Pure: same argv and cwd, same profile.
pub fn host_launch_profile(argv:&[String],cwd:&str)->Result<RpcLaunchProfile,serde_json::Error>{
    let mut extensions=Vec::new();let mut multi_session=false;let mut session_runtime=crate::host_protocol_info::SessionRuntimeKind::InProcess;
    let mut index=0;
    while index<argv.len(){
        match argv[index].as_str(){
            "--extension"=>{if let Some(value)=argv.get(index+1){extensions.push(std::path::Path::new(cwd).join(value).to_string_lossy().into_owned());}index+=2;continue;},
            "--multi-session"=>{multi_session=true;},
            "--session-runtime"=>{if let Some(value)=argv.get(index+1){session_runtime=if value=="worker"{crate::host_protocol_info::SessionRuntimeKind::Worker}else{crate::host_protocol_info::SessionRuntimeKind::InProcess};}index+=2;continue;},
            _=>{}
        }
        index+=1;
    }
    launch_profile_from_core(RpcLaunchProfileCore{extensions,multi_session,session_runtime})
}
pub fn protocol_identity(profile:RpcLaunchProfile,env:&HashMap<String,String>)->serde_json::Value{let build=maho_core::engine_build_identity::engine_build_identity();serde_json::json!({"instanceId":host_instance_id(),"generation":host_generation(env),"engineVersion":build.text,"engineOrdinal":build.ordinal,"launch_profile":profile})}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn launch_profile_derives_from_argv_and_resolves_extensions(){let profile=host_launch_profile(&["--extension","a.js","--extension","./b.js","--multi-session","--session-runtime","worker"].map(str::to_owned),"/root").unwrap();assert_eq!(profile.core.extensions,vec!["/root/a.js","/root/b.js"]);assert!(profile.core.multi_session);assert_eq!(profile.core.session_runtime,crate::host_protocol_info::SessionRuntimeKind::Worker);assert_eq!(host_launch_profile(&[].map(str::to_owned),"/root").unwrap().core.session_runtime,crate::host_protocol_info::SessionRuntimeKind::InProcess);}
    #[test]fn generation_rejects_sign_whitespace_decimal_and_empty(){for value in ["-1"," 1","1.0","","+1"]{assert_eq!(host_generation(&HashMap::from([(HOST_GENERATION_ENV.into(),value.into())])),0.);}assert_eq!(host_generation(&HashMap::from([(HOST_GENERATION_ENV.into(),"0012".into())])),12.);}
    #[test]fn instance_value_preserves_nonempty_spelling(){assert_eq!(resolve_instance_id(Some(" id "))," id ");assert!(uuid::Uuid::parse_str(&resolve_instance_id(Some(" "))).is_ok());assert_eq!(host_instance_id(),host_instance_id());}
    #[test]fn canonical_profile_sorts_and_deduplicates(){use crate::host_protocol_info::SessionRuntimeKind;let profile=launch_profile_from_core(RpcLaunchProfileCore{extensions:vec!["b".into(),"a".into(),"a".into()],multi_session:true,session_runtime:SessionRuntimeKind::Worker}).unwrap();let canonical=br#"{"extensions":["a","b"],"multi_session":true,"session_runtime":"worker"}"#;assert_eq!(profile.profile_id,format!("{:x}",Sha256::digest(canonical)));}
}
