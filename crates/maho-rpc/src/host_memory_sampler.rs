use std::collections::HashMap;
pub const HOST_RSS_WARN_MB_ENV:&str="SENPI_RPC_HOST_RSS_WARN_MB";
pub const DEFAULT_HOST_RSS_WARN_MB:u64=4096;
pub const HOST_RSS_REFUSE_MB_ENV:&str="SENPI_RPC_HOST_RSS_REFUSE_MB";
pub const HOST_MEMORY_SAMPLE_MS:u64=30000;
pub const HOST_MEMORY_STDERR_INTERVAL_MS:u64=300000;
#[derive(Debug,Default,PartialEq)]
pub struct MemorySample {pub pressure_change:Option<bool>,pub critical_change:Option<(bool,u64)>,pub idle_pressure:Option<u64>,pub record:Option<serde_json::Value>,pub log:Option<String>}
pub struct HostMemorySampler {warn_mb:u64,refuse_mb:u64,pressure:bool,critical:bool,idle_reported:bool,last_logged_at:Option<u64>}
fn positive_integer(value:Option<&String>)->Option<u64>{let text=value?.trim();if text.is_empty()||!text.bytes().all(|c|c.is_ascii_digit()){return None;}text.parse().ok().filter(|value|*value>0)}
impl HostMemorySampler{
    pub async fn run_until_stopped(&mut self,mut read:impl FnMut()->(u64,u64,u64),mut publish:impl FnMut(MemorySample),mut stopped:tokio::sync::watch::Receiver<bool>){
        let period=std::time::Duration::from_millis(HOST_MEMORY_SAMPLE_MS);
        let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{
            if *stopped.borrow(){return;}
            tokio::select!{
                changed=stopped.changed()=>{if changed.is_err(){return;}},
                _=timer.tick()=>{let(rss,sessions,now)=read();publish(self.sample(rss,sessions,now));}
            }
        }
    }
    pub async fn run(&mut self,mut read:impl FnMut()->(u64,u64,u64),mut publish:impl FnMut(MemorySample)){
        let period=std::time::Duration::from_millis(HOST_MEMORY_SAMPLE_MS);
        let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{timer.tick().await;let(rss,sessions,now)=read();publish(self.sample(rss,sessions,now));}
    }
    pub fn new(env:&HashMap<String,String>)->Self{let warn_mb=positive_integer(env.get(HOST_RSS_WARN_MB_ENV)).unwrap_or(DEFAULT_HOST_RSS_WARN_MB);let refuse_mb=positive_integer(env.get(HOST_RSS_REFUSE_MB_ENV)).unwrap_or(warn_mb*2);Self{warn_mb,refuse_mb,pressure:false,critical:false,idle_reported:false,last_logged_at:None}}
    pub fn sample(&mut self,rss_bytes:u64,sessions:u64,now:u64)->MemorySample{
        let rss_mb=(rss_bytes+524288)/1048576;let critical=rss_mb>self.refuse_mb;let mut result=MemorySample::default();
        if critical!=self.critical{self.critical=critical;result.critical_change=Some((critical,rss_mb));}
        if rss_mb<=self.warn_mb{self.idle_reported=false;if self.pressure{self.pressure=false;result.pressure_change=Some(false);}return result;}
        if !self.pressure{self.pressure=true;result.pressure_change=Some(true);}
        if sessions>0{self.idle_reported=false;}else if !self.idle_reported{self.idle_reported=true;result.idle_pressure=Some(rss_mb);}
        result.record=Some(serde_json::json!({"type":"host_memory_pressure","rssMb":rss_mb,"sessions":sessions}));
        if self.last_logged_at.is_some_and(|last|now.saturating_sub(last)<HOST_MEMORY_STDERR_INTERVAL_MS){return result;}
        self.last_logged_at=Some(now);let policy=if critical{"idle parking halved, new worker sessions refused"}else{"idle parking halved"};
        result.log=Some(format!("senpi rpc host memory pressure: rssMb={rss_mb} sessions={sessions} ({policy})\n"));result
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    fn env()->HashMap<String,String>{HashMap::from([(HOST_RSS_WARN_MB_ENV.into(),"10".into())])}
    #[test] fn thresholds_are_strict_and_critical_transitions_are_independent(){let mut sampler=HostMemorySampler::new(&env());assert_eq!(sampler.sample(10*1048576,0,0),MemorySample::default());let sample=sampler.sample(21*1048576,1,1);assert_eq!(sample.pressure_change,Some(true));assert_eq!(sample.critical_change,Some((true,21)));let sample=sampler.sample(20*1048576,1,2);assert_eq!(sample.critical_change,Some((false,20)));assert!(sample.record.is_some());assert_eq!(sampler.sample(10*1048576,1,3).pressure_change,Some(false));}
    #[test] fn idle_episode_restarts_after_a_session_or_pressure_recovery(){let mut sampler=HostMemorySampler::new(&env());assert_eq!(sampler.sample(11*1048576,0,0).idle_pressure,Some(11));assert_eq!(sampler.sample(11*1048576,0,1).idle_pressure,None);sampler.sample(11*1048576,1,2);assert_eq!(sampler.sample(11*1048576,0,3).idle_pressure,Some(11));sampler.sample(0,0,4);assert_eq!(sampler.sample(11*1048576,0,5).idle_pressure,Some(11));}
    #[test] fn logs_are_throttled_but_records_are_not(){let mut sampler=HostMemorySampler::new(&env());assert!(sampler.sample(11*1048576,0,0).log.is_some());let sample=sampler.sample(11*1048576,0,299999);assert!(sample.record.is_some());assert!(sample.log.is_none());assert!(sampler.sample(11*1048576,0,300000).log.is_some());}
    #[test] fn malformed_thresholds_use_default(){let mut sampler=HostMemorySampler::new(&HashMap::from([(HOST_RSS_WARN_MB_ENV.into(),"1e1".into())]));assert!(sampler.sample(11*1048576,0,0).record.is_none());}
}
