use std::collections::HashMap;
pub const RPC_SESSION_IDLE_EVICTION_MS_ENV:&str="SENPI_RPC_SESSION_IDLE_EVICTION_MS";
pub const RPC_HOST_EMPTY_EXIT_MS_ENV:&str="SENPI_RPC_HOST_EMPTY_EXIT_MS";
pub const RPC_CLOSE_GRACE_MS_ENV:&str="SENPI_RPC_CLOSE_GRACE_MS";
pub const DEFAULT_SESSION_IDLE_EVICTION_MS:f64=30.*60_000.;
pub const DEFAULT_HOST_EMPTY_EXIT_MS:f64=15.*60_000.;
#[derive(Debug,PartialEq)]pub struct HostIdlePolicy{pub idle_eviction_ms:f64,pub empty_exit_ms:f64}
pub fn resolve_host_idle_policy(env:&HashMap<String,String>,idle_eviction_ms:Option<f64>,empty_exit_ms:Option<f64>)->HostIdlePolicy{
    HostIdlePolicy{idle_eviction_ms:idle_eviction_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_SESSION_IDLE_EVICTION_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_SESSION_IDLE_EVICTION_MS),empty_exit_ms:empty_exit_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_HOST_EMPTY_EXIT_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_HOST_EMPTY_EXIT_MS)}
}
#[cfg(test)]mod tests{use super::*;#[test]fn explicit_policy_overrides_environment_and_bad_env_uses_defaults(){let env=HashMap::from([(RPC_SESSION_IDLE_EVICTION_MS_ENV.into(),"100".into()),(RPC_HOST_EMPTY_EXIT_MS_ENV.into(),"invalid".into())]);assert_eq!(resolve_host_idle_policy(&env,None,None),HostIdlePolicy{idle_eviction_ms:100.,empty_exit_ms:DEFAULT_HOST_EMPTY_EXIT_MS});assert_eq!(resolve_host_idle_policy(&env,Some(f64::INFINITY),Some(0.)),HostIdlePolicy{idle_eviction_ms:f64::INFINITY,empty_exit_ms:0.});}}
