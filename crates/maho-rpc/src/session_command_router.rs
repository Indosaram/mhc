pub struct IdleSession{pub session_id:String,pub open:bool,pub busy:bool,pub last_command_at:f64}
pub struct IdleSweep{pub evict:Vec<String>,pub exit:bool}
pub struct SessionIdlePolicy{pub idle_eviction_ms:f64,pub empty_exit_ms:f64,pub memory_pressure:bool,empty_since:Option<f64>,stopped:bool}
impl SessionIdlePolicy{
    pub fn new(idle_eviction_ms:f64,empty_exit_ms:f64)->Self{Self{idle_eviction_ms,empty_exit_ms,memory_pressure:false,empty_since:None,stopped:false}}
    pub fn sweep(&mut self,now:f64,sessions:&mut[IdleSession],registry_size:usize,can_exit:bool)->IdleSweep{
        if self.stopped{return IdleSweep{evict:vec![],exit:false};}
        let mut evict=vec![];let eviction=if self.memory_pressure{self.idle_eviction_ms/2.}else{self.idle_eviction_ms};
        if eviction.is_finite(){for entry in sessions{
            if !entry.open{continue;}
            if entry.busy{entry.last_command_at=now;continue;}
            if now-entry.last_command_at>=eviction{evict.push(entry.session_id.clone());}
        }}
        let mut exit=false;
        if self.empty_exit_ms.is_finite(){if registry_size==0&&can_exit{if let Some(since)=self.empty_since{if now-since>=self.empty_exit_ms{self.stopped=true;exit=true;}}else{self.empty_since=Some(now);}}else{self.empty_since=None;}}
        IdleSweep{evict,exit}
    }
}
#[cfg(test)]mod tests{use super::*;#[test]fn busy_work_resets_idle_window_and_pressure_halves_it(){let mut policy=SessionIdlePolicy::new(100.,100.);let mut sessions=[IdleSession{session_id:"s".into(),open:true,busy:true,last_command_at:0.}];assert!(policy.sweep(200.,&mut sessions,1,true).evict.is_empty());assert_eq!(sessions[0].last_command_at,200.);sessions[0].busy=false;policy.memory_pressure=true;assert!(policy.sweep(249.,&mut sessions,1,true).evict.is_empty());assert_eq!(policy.sweep(250.,&mut sessions,1,true).evict,vec!["s"]);}#[test]fn connected_client_resets_empty_exit_and_exit_fires_once(){let mut policy=SessionIdlePolicy::new(f64::INFINITY,100.);assert!(!policy.sweep(0.,&mut[],0,true).exit);assert!(!policy.sweep(100.,&mut[],0,false).exit);assert!(!policy.sweep(200.,&mut[],0,true).exit);assert!(policy.sweep(300.,&mut[],0,true).exit);assert!(!policy.sweep(400.,&mut[],0,true).exit);}}
