use std::collections::HashMap;
use crate::child_reaper_syscalls::ChildReaperSyscalls;
pub const CHILD_REAPER_ENV:&str="SENPI_RPC_HOST_REAPER";
pub const CHILD_REAPER_MIN_WAITABLE_MS_ENV:&str="SENPI_RPC_HOST_REAPER_MIN_WAITABLE_MS";
pub const MIN_WAITABLE_FLOOR_MS:u64=5000;
#[derive(Debug,PartialEq,Eq)]
pub struct ChildReaperConfig { pub enabled:bool,pub tick_ms:u64,pub min_waitable_ms:u64 }
pub fn resolve_child_reaper_config(env:&HashMap<String,String>) -> ChildReaperConfig {
    let window=env.get(CHILD_REAPER_MIN_WAITABLE_MS_ENV).and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v>0).unwrap_or(30000).max(MIN_WAITABLE_FLOOR_MS);
    ChildReaperConfig { enabled:env.get(CHILD_REAPER_ENV).is_none_or(|v| v!="0"),tick_ms:1000,min_waitable_ms:window }
}
struct TrackedChild { pid:i32,name:String,waitable_since:Option<u64> }
pub struct ChildReaper<S> { pub syscalls:S,tracked:Vec<TrackedChild>,min_waitable_ms:u64,last_warn_at:Option<u64> }
impl<S:ChildReaperSyscalls> ChildReaper<S> {
    pub async fn run(&mut self,tick_ms:u64,mut now:impl FnMut()->u64,mut log:impl FnMut(String)){
        let period=std::time::Duration::from_millis(tick_ms);
        let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{timer.tick().await;if let Some(message)=self.tick(now()){log(message);}}
    }
    pub fn new(syscalls:S,min_waitable_ms:u64) -> Self { Self { syscalls,tracked:Vec::new(),min_waitable_ms:min_waitable_ms.max(MIN_WAITABLE_FLOOR_MS),last_warn_at:None } }
    pub fn waiting_pids(&self) -> Vec<i32> { self.tracked.iter().filter(|c| c.waitable_since.is_some()).map(|c| c.pid).collect() }
    pub fn tick(&mut self,now:u64) -> Option<String> {
        let children=self.syscalls.list_direct_children();self.tracked.retain(|c| children.contains(&c.pid));
        let mut reaped=Vec::new();
        for pid in children {
            let index=match self.tracked.iter().position(|c| c.pid==pid) { Some(index)=>index,None=>{ let name=self.syscalls.describe(pid);self.tracked.push(TrackedChild { pid,name:if name.is_empty(){"unknown".into()}else{name},waitable_since:None });self.tracked.len()-1 } };
            let child=&mut self.tracked[index];
            if !self.syscalls.is_waitable(pid) { child.waitable_since=None;continue; }
            let Some(since)=child.waitable_since else { child.waitable_since=Some(now);continue; };
            if now.saturating_sub(since)<self.min_waitable_ms || !self.syscalls.reap_exited(pid) { continue; }
            reaped.push(child.name.clone());self.tracked.remove(index);
        }
        let waiting=self.tracked.iter().filter(|c| c.waitable_since.is_some()).map(|c| c.name.as_str()).collect::<Vec<_>>();
        if (reaped.is_empty() && waiting.len()<10) || self.last_warn_at.is_some_and(|at| now.saturating_sub(at)<300000) { return None; }
        self.last_warn_at=Some(now);
        let mut counts:Vec<(&str,usize)>=Vec::new();
        for name in waiting.iter().copied().chain(reaped.iter().map(String::as_str)) { if let Some((_,count))=counts.iter_mut().find(|(key,_)| *key==name) { *count+=1; }else{ counts.push((name,1)); } }
        counts.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let top=counts.iter().take(3).map(|(name,count)| format!("{name} x{count}")).collect::<Vec<_>>().join(", ");
        Some(format!("child reaper: reaped={} waiting={} top={top}",reaped.len(),waiting.len()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fake { children:Vec<(i32,String,bool)>,reaped:Vec<i32> }
    impl ChildReaperSyscalls for Fake {
        fn list_direct_children(&mut self)->Vec<i32>{self.children.iter().map(|c|c.0).collect()}
        fn is_waitable(&mut self,pid:i32)->bool{self.children.iter().any(|c|c.0==pid&&c.2)}
        fn reap_exited(&mut self,pid:i32)->bool{self.reaped.push(pid);self.children.retain(|c|c.0!=pid);true}
        fn describe(&mut self,pid:i32)->String{self.children.iter().find(|c|c.0==pid).unwrap().1.clone()}
    }
    #[test] fn waits_full_window_across_ticks(){let mut r=ChildReaper::new(Fake{children:vec![(4242,"sleep".into(),true)],reaped:vec![]},5000);r.tick(0);r.tick(4999);assert!(r.syscalls.reaped.is_empty());assert_eq!(r.waiting_pids(),vec![4242]);r.tick(5001);assert_eq!(r.syscalls.reaped,vec![4242]);}
    #[test] fn never_reaps_live_child(){let mut r=ChildReaper::new(Fake{children:vec![(77,"bash".into(),false)],reaped:vec![]},5000);r.tick(0);r.tick(60000);assert!(r.syscalls.reaped.is_empty());assert!(r.waiting_pids().is_empty());}
    #[test] fn warning_is_rate_limited_and_top_three_are_stable(){let mut children=Vec::new();for i in 0..6{children.push((100+i,"sleep".into(),true));}for i in 0..4{children.push((200+i,"rg".into(),true));}children.push((300,"git".into(),true));children.push((301,"curl".into(),true));let mut r=ChildReaper::new(Fake{children,reaped:vec![]},5000);let warning=r.tick(0).unwrap();assert!(warning.contains("waiting=12"));assert!(!warning.contains("curl"));assert!(r.tick(1000).is_none());}
    #[test] fn optout_and_default_window(){assert!(!resolve_child_reaper_config(&HashMap::from([(CHILD_REAPER_ENV.into(),"0".into())])).enabled);let config=resolve_child_reaper_config(&HashMap::new());assert!(config.enabled);assert_eq!(config.tick_ms,1000);assert_eq!(config.min_waitable_ms,30000);}
}
