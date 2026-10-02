pub struct IdleSession{pub session_id:String,pub open:bool,pub busy:bool,pub last_command_at:f64}
pub struct IdleSweep{pub evict:Vec<String>,pub exit:bool}
#[derive(Default)]pub struct ConnectionAttachments{owned:std::collections::BTreeMap<String,std::collections::BTreeMap<String,usize>>}
impl ConnectionAttachments{
    pub fn attach(&mut self,owner:&str,session:&str){*self.owned.entry(owner.into()).or_default().entry(session.into()).or_default()+=1;}
    pub fn owns(&self,owner:Option<&str>,session:&str)->bool{owner.is_none_or(|owner|self.owned.get(owner).is_some_and(|sessions|sessions.contains_key(session)))}
    pub fn admit_close(&mut self,route:(Option<&str>,&str),response:&serde_json::Value,entry:&mut crate::session_registry::SessionCloseState,detach:bool,writer:&crate::session_event_writer::SessionWriterActor)->Result<Option<(u64,crate::session_teardown::CloseClaim)>,String>{
        let(owner,session)=route;
        if !self.owns(owner,session){return Err("unknown_session".into());}
        let claim=crate::session_teardown::admit_session_close(writer,session,response,entry,detach)?;
        if claim.is_some()&&let Some(owner)=owner{self.release(owner,session);}
        Ok(claim)
    }
    pub fn release(&mut self,owner:&str,session:&str){
        let Some(sessions)=self.owned.get_mut(owner)else{return;};
        let Some(count)=sessions.get_mut(session)else{return;};
        if *count==1{sessions.remove(session);}else{*count-=1;}
        if sessions.is_empty(){self.owned.remove(owner);}
    }
}
#[cfg(test)]mod attachment_tests{use super::*;#[test]fn public_handle_does_not_authorize_another_connections_close(){let mut attachments=ConnectionAttachments::default();attachments.attach("one","s");attachments.attach("one","s");assert!(!attachments.owns(Some("two"),"s"));assert!(attachments.owns(None,"s"));attachments.release("two","s");assert!(attachments.owns(Some("one"),"s"));attachments.release("one","s");assert!(attachments.owns(Some("one"),"s"));attachments.release("one","s");assert!(!attachments.owns(Some("one"),"s"));}}
#[derive(Default)]pub struct SharedSessionWidths{widths:std::collections::BTreeMap<String,std::collections::BTreeMap<String,f64>>}
impl SharedSessionWidths{
    pub fn set_width(&mut self,session:&str,connection:Option<&str>,width:f64){if let Some(connection)=connection{self.widths.entry(session.into()).or_default().insert(connection.into(),width);}}
    pub fn clear_width(&mut self,session:&str,connection:Option<&str>){if let Some(connection)=connection&&let Some(widths)=self.widths.get_mut(session){widths.remove(connection);}}
    pub fn width(&self,session:&str)->f64{self.widths.get(session).and_then(|widths|widths.values().copied().reduce(f64::min)).unwrap_or(80.)}
}
#[cfg(test)]mod width_tests{use super::*;#[test]fn narrowest_live_connection_controls_render_width(){let mut widths=SharedSessionWidths::default();assert_eq!(widths.width("s"),80.);widths.set_width("s",None,10.);widths.set_width("s",Some("desktop"),120.);widths.set_width("s",Some("phone"),40.);assert_eq!(widths.width("s"),40.);widths.clear_width("s",Some("phone"));assert_eq!(widths.width("s"),120.);widths.clear_width("s",Some("desktop"));assert_eq!(widths.width("s"),80.);}}
pub struct SessionIdlePolicy{pub idle_eviction_ms:f64,pub empty_exit_ms:f64,pub memory_pressure:bool,empty_since:Option<f64>,stopped:bool}
impl SessionIdlePolicy{
    pub fn new(idle_eviction_ms:f64,empty_exit_ms:f64)->Self{Self{idle_eviction_ms,empty_exit_ms,memory_pressure:false,empty_since:None,stopped:false}}
    pub async fn run(&mut self,mut sample:impl FnMut()->(f64,Vec<IdleSession>,usize,bool),mut publish:impl FnMut(IdleSweep,Vec<IdleSession>),mut stopped:tokio::sync::watch::Receiver<bool>){
        if !self.idle_eviction_ms.is_finite()&&!self.empty_exit_ms.is_finite(){let _=stopped.wait_for(|stop|*stop).await;return;}
        let period=std::time::Duration::from_secs_f64((self.idle_eviction_ms.min(self.empty_exit_ms)/4.).clamp(20.,5000.)/1000.);
        let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{
            if *stopped.borrow(){return;}
            tokio::select!{
                changed=stopped.changed()=>{if changed.is_err(){return;}},
                _=timer.tick()=>{
                    let(now,mut sessions,size,can_exit)=sample();
                    let sweep=self.sweep(now,&mut sessions,size,can_exit);let exit=sweep.exit;
                    publish(sweep,sessions);if exit{return;}
                }
            }
        }
    }
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
