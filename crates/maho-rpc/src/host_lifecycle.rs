use std::collections::HashMap;
pub use crate::host_child_exit::classify_child_exit;
pub const HOST_COLD_START_ENV:&str="SENPI_RPC_HOST_COLD_START";
pub const HOST_IDLE_EXIT_MS_ENV:&str="SENPI_RPC_HOST_IDLE_EXIT_MS";
pub const DEFAULT_HOST_IDLE_EXIT_MS:f64=900000.;
pub const HANDOFF_GRACE_MS_ENV:&str="SENPI_RPC_HANDOFF_GRACE_MS";
pub const DEFAULT_HANDOFF_GRACE_MS:f64=600000.;
pub use crate::host_launch_spec::HostLifecyclePolicyInput;
pub const INTERNAL_SUPERVISOR_FLAG:&str="--internal-rpc-host-supervisor";
#[derive(Debug,PartialEq,Eq)]
pub struct SupervisorLaunch{pub socket:String,pub host_args:Vec<String>,pub child_command:Option<String>,pub child_args:Option<Vec<String>>,pub agent_dir:Option<String>,pub bind_socket:Option<String>,pub replace_identity:Option<crate::socket_ownership::SocketFileIdentity>}
pub fn find_internal_supervisor_args(argv:&[String])->Option<&[String]>{
    let mut index=0;
    while index<argv.len(){
        if argv[index]==INTERNAL_SUPERVISOR_FLAG{return Some(&argv[index+1..]);}
        if argv[index]!="--extension"||index+1>=argv.len(){return None;}
        index+=2;
    }
    None
}
pub fn parse_supervisor_args(argv:&[String])->Option<SupervisorLaunch>{
    let mut launch=SupervisorLaunch{socket:String::new(),host_args:vec![],child_command:None,child_args:None,agent_dir:None,bind_socket:None,replace_identity:None};
    let mut socket_seen=false;let mut index=0;
    while index<argv.len(){
        let arg=&argv[index];
        if index+1<argv.len()&&matches!(arg.as_str(),"--socket"|"--child-command"|"--child-args"|"--agent-dir"|"--bind"|"--replace"){
            index+=1;let value=&argv[index];
            match arg.as_str(){
                "--socket"=>{launch.socket=value.clone();socket_seen=true;},
                "--child-command"=>launch.child_command=Some(value.clone()),
                "--child-args"=>{let parsed:serde_json::Value=serde_json::from_str(value).ok()?;launch.child_args=parsed.as_array().and_then(|items|items.iter().map(|item|item.as_str().map(str::to_owned)).collect());},
                "--agent-dir"=>launch.agent_dir=Some(value.clone()),
                "--bind"=>launch.bind_socket=Some(value.clone()),
                "--replace"=>launch.replace_identity=value.split_once(':').filter(|(dev,ino)|!dev.is_empty()&&!ino.is_empty()&&dev.bytes().chain(ino.bytes()).all(|byte|byte.is_ascii_digit())).and_then(|(dev,ino)|Some(crate::socket_ownership::SocketFileIdentity{dev:dev.parse().ok()?,ino:ino.parse().ok()?})),
                _=>unreachable!(),
            }
        }else{launch.host_args.push(arg.clone());}
        index+=1;
    }
    socket_seen.then_some(launch)
}
#[derive(Debug,PartialEq)]
pub struct HostLifecyclePolicy{pub cold_start:String,pub idle_exit_ms:f64}
pub fn parse_cold_start(value:Option<&str>)->Option<&str>{value.filter(|value|matches!(*value,"transient"|"persistent"))}
pub fn parse_idle_exit_ms(value:Option<&str>)->Option<f64>{let value=value?.trim();if value.is_empty()||!value.bytes().all(|byte|byte.is_ascii_digit()){return None;}value.parse::<f64>().ok().filter(|number|number.is_finite()&&*number>0.)}
pub fn resolve_host_policy(settings:&serde_json::Value,env:&HashMap<String,String>)->HostLifecyclePolicy{
    let cold_start=parse_cold_start(env.get(HOST_COLD_START_ENV).map(String::as_str)).or_else(||parse_cold_start(settings.get("coldStart").and_then(serde_json::Value::as_str))).unwrap_or("transient").into();
    let setting_idle=settings.get("idleExitMs").and_then(|value|if value.is_string(){value.as_str().map(str::to_owned)}else if value.is_number(){Some(value.to_string())}else{None});
    let idle_exit_ms=parse_idle_exit_ms(env.get(HOST_IDLE_EXIT_MS_ENV).map(String::as_str)).or_else(||parse_idle_exit_ms(setting_idle.as_deref())).unwrap_or(DEFAULT_HOST_IDLE_EXIT_MS);HostLifecyclePolicy{cold_start,idle_exit_ms}
}
#[derive(Debug,PartialEq,Eq)]pub enum IdleExitDecision{Active,Idle,Exit}
pub struct IdleExitDecider{pub idle_exit_ms:f64,idle_since:Option<f64>}
impl IdleExitDecider{
    pub fn new(idle_exit_ms:f64)->Self{Self{idle_exit_ms,idle_since:None}}
    pub fn update(&mut self,connections:u64,active_turns:u64,now:f64)->IdleExitDecision{
        if connections>0||active_turns>0{self.idle_since=None;return IdleExitDecision::Active;}
        if self.idle_exit_ms==f64::INFINITY{return IdleExitDecision::Idle;}
        let since=*self.idle_since.get_or_insert(now);if now-since>=self.idle_exit_ms{IdleExitDecision::Exit}else{IdleExitDecision::Idle}
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn supervisor_route_skips_only_complete_injected_prefix(){let args=["--extension","dir",INTERNAL_SUPERVISOR_FLAG,"--socket","sock"].map(str::to_owned);assert_eq!(find_internal_supervisor_args(&args),Some(&args[3..]));assert!(find_internal_supervisor_args(&["--model".into(),INTERNAL_SUPERVISOR_FLAG.into()]).is_none());assert!(find_internal_supervisor_args(&["--extension".into(),INTERNAL_SUPERVISOR_FLAG.into()]).is_none());}
    #[test]fn supervisor_parser_keeps_forwarded_args_and_replacement_identity(){let args=["--socket","sock","--replace","1:2","--child-args","[\"one\"]","--mode","rpc"].map(str::to_owned);let launch=parse_supervisor_args(&args).unwrap();assert_eq!(launch.child_args,Some(vec!["one".into()]));assert_eq!(launch.host_args,vec!["--mode","rpc"]);assert_eq!(launch.replace_identity,Some(crate::socket_ownership::SocketFileIdentity{dev:1,ino:2}));assert!(parse_supervisor_args(&["--socket".into(),"sock".into(),"--child-args".into(),"{".into()]).is_none());}
    #[test]fn continuous_idle_resets_on_activity(){let mut decider=IdleExitDecider::new(100.);assert_eq!(decider.update(0,0,0.),IdleExitDecision::Idle);assert_eq!(decider.update(0,1,99.),IdleExitDecision::Active);assert_eq!(decider.update(0,0,100.),IdleExitDecision::Idle);assert_eq!(decider.update(0,0,200.),IdleExitDecision::Exit);}
    #[test]fn policy_precedence_and_invalid_fallback(){let settings=serde_json::json!({"coldStart":"persistent","idleExitMs":1000});let env=HashMap::from([(HOST_COLD_START_ENV.into(),"invalid".into()),(HOST_IDLE_EXIT_MS_ENV.into()," 2000 ".into())]);assert_eq!(resolve_host_policy(&settings,&env),HostLifecyclePolicy{cold_start:"persistent".into(),idle_exit_ms:2000.});assert!(parse_idle_exit_ms(Some("1e3")).is_none());}
    #[test]fn persistent_never_idle_exits(){assert_eq!(IdleExitDecider::new(f64::INFINITY).update(0,0,100000000.),IdleExitDecision::Idle);}
}
