use std::collections::HashMap;
pub const RPC_SESSION_IDLE_EVICTION_MS_ENV:&str="SENPI_RPC_SESSION_IDLE_EVICTION_MS";
pub const RPC_HOST_EMPTY_EXIT_MS_ENV:&str="SENPI_RPC_HOST_EMPTY_EXIT_MS";
pub const RPC_CLOSE_GRACE_MS_ENV:&str="SENPI_RPC_CLOSE_GRACE_MS";
pub const DEFAULT_SESSION_IDLE_EVICTION_MS:f64=30.*60_000.;
pub const DEFAULT_HOST_EMPTY_EXIT_MS:f64=15.*60_000.;
#[derive(Debug,PartialEq)]pub struct HostIdlePolicy{pub idle_eviction_ms:f64,pub empty_exit_ms:f64}
pub enum HostMonitorSample{Lag(crate::loop_lag_watchdog::LagSample),Memory(crate::host_memory_sampler::MemorySample)}
pub async fn run_host_monitors(env:&HashMap<String,String>,activity:&crate::session_attribution::SessionActivityRegistry,blocked:&std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>,mut memory:impl FnMut()->(u64,u64,u64),mut now:impl FnMut()->f64,publish:impl FnMut(HostMonitorSample),stopped:tokio::sync::watch::Receiver<bool>){
    let publish=std::sync::Mutex::new(publish);
    let mut lag=crate::loop_lag_watchdog::LoopLagWatchdog::new(env);
    let mut rss=crate::host_memory_sampler::HostMemorySampler::new(env);
    let stall=lag.run_shared(activity,blocked,&mut now,|sample|{publish.lock().unwrap_or_else(std::sync::PoisonError::into_inner)(HostMonitorSample::Lag(sample));},stopped.clone());
    let pressure=rss.run_until_stopped(&mut memory,|sample|{publish.lock().unwrap_or_else(std::sync::PoisonError::into_inner)(HostMonitorSample::Memory(sample));},stopped);
    tokio::join!(stall,pressure);
}
pub fn resolve_host_idle_policy(env:&HashMap<String,String>,idle_eviction_ms:Option<f64>,empty_exit_ms:Option<f64>)->HostIdlePolicy{
    HostIdlePolicy{idle_eviction_ms:idle_eviction_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_SESSION_IDLE_EVICTION_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_SESSION_IDLE_EVICTION_MS),empty_exit_ms:empty_exit_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_HOST_EMPTY_EXIT_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_HOST_EMPTY_EXIT_MS)}
}
#[cfg(test)]mod tests{use super::*;#[test]fn explicit_policy_overrides_environment_and_bad_env_uses_defaults(){let env=HashMap::from([(RPC_SESSION_IDLE_EVICTION_MS_ENV.into(),"100".into()),(RPC_HOST_EMPTY_EXIT_MS_ENV.into(),"invalid".into())]);assert_eq!(resolve_host_idle_policy(&env,None,None),HostIdlePolicy{idle_eviction_ms:100.,empty_exit_ms:DEFAULT_HOST_EMPTY_EXIT_MS});assert_eq!(resolve_host_idle_policy(&env,Some(f64::INFINITY),Some(0.)),HostIdlePolicy{idle_eviction_ms:f64::INFINITY,empty_exit_ms:0.});}}
