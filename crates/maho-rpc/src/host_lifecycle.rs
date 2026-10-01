use std::collections::HashMap;
pub use crate::host_child_exit::classify_child_exit;
pub const HOST_COLD_START_ENV:&str="SENPI_RPC_HOST_COLD_START";
pub const HOST_IDLE_EXIT_MS_ENV:&str="SENPI_RPC_HOST_IDLE_EXIT_MS";
pub const DEFAULT_HOST_IDLE_EXIT_MS:f64=900000.;
pub const HANDOFF_GRACE_MS_ENV:&str="SENPI_RPC_HANDOFF_GRACE_MS";
pub const DEFAULT_HANDOFF_GRACE_MS:f64=600000.;
pub use crate::host_launch_spec::HostLifecyclePolicyInput;
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
    #[test]fn continuous_idle_resets_on_activity(){let mut decider=IdleExitDecider::new(100.);assert_eq!(decider.update(0,0,0.),IdleExitDecision::Idle);assert_eq!(decider.update(0,1,99.),IdleExitDecision::Active);assert_eq!(decider.update(0,0,100.),IdleExitDecision::Idle);assert_eq!(decider.update(0,0,200.),IdleExitDecision::Exit);}
    #[test]fn policy_precedence_and_invalid_fallback(){let settings=serde_json::json!({"coldStart":"persistent","idleExitMs":1000});let env=HashMap::from([(HOST_COLD_START_ENV.into(),"invalid".into()),(HOST_IDLE_EXIT_MS_ENV.into()," 2000 ".into())]);assert_eq!(resolve_host_policy(&settings,&env),HostLifecyclePolicy{cold_start:"persistent".into(),idle_exit_ms:2000.});assert!(parse_idle_exit_ms(Some("1e3")).is_none());}
    #[test]fn persistent_never_idle_exits(){assert_eq!(IdleExitDecider::new(f64::INFINITY).update(0,0,100000000.),IdleExitDecision::Idle);}
}
