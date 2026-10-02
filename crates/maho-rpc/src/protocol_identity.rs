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
pub fn protocol_identity(profile:RpcLaunchProfile,env:&HashMap<String,String>)->serde_json::Value{let build=maho_core::engine_build_identity::engine_build_identity();serde_json::json!({"instanceId":host_instance_id(),"generation":host_generation(env),"engineVersion":build.text,"engineOrdinal":build.ordinal,"launch_profile":profile})}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn generation_rejects_sign_whitespace_decimal_and_empty(){for value in ["-1"," 1","1.0","","+1"]{assert_eq!(host_generation(&HashMap::from([(HOST_GENERATION_ENV.into(),value.into())])),0.);}assert_eq!(host_generation(&HashMap::from([(HOST_GENERATION_ENV.into(),"0012".into())])),12.);}
    #[test]fn instance_value_preserves_nonempty_spelling(){assert_eq!(resolve_instance_id(Some(" id "))," id ");assert!(uuid::Uuid::parse_str(&resolve_instance_id(Some(" "))).is_ok());assert_eq!(host_instance_id(),host_instance_id());}
    #[test]fn canonical_profile_sorts_and_deduplicates(){use crate::host_protocol_info::SessionRuntimeKind;let profile=launch_profile_from_core(RpcLaunchProfileCore{extensions:vec!["b".into(),"a".into(),"a".into()],multi_session:true,session_runtime:SessionRuntimeKind::Worker}).unwrap();let canonical=br#"{"extensions":["a","b"],"multi_session":true,"session_runtime":"worker"}"#;assert_eq!(profile.profile_id,format!("{:x}",Sha256::digest(canonical)));}
}
