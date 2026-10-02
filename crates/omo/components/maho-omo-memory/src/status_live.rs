pub const FRAME_INTERVAL_MS:u64=320;
pub const MEMORY_REFLECTING_FRAMES:[&str;10]=["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
pub trait MemoryFooterUi{fn set_status(&mut self,key:&str,text:&str);}
pub trait MemoryFooterProbe{fn fingerprint(&mut self,identity:&str)->Result<Option<String>,String>;fn refresh_segments(&mut self,identity:&str)->Result<Option<String>,String>;}
#[derive(Default)]pub struct MemoryFooterLive{frame_index:usize,active_identity:Option<String>,disposed:bool,last_fingerprint:Option<String>}
impl MemoryFooterLive{
    pub fn set_active(&mut self,identity:Option<&str>,active:bool,ui:Option<&mut dyn MemoryFooterUi>,probe:&mut dyn MemoryFooterProbe){
        if self.disposed{return;}
        if active{let (Some(identity),Some(ui))=(identity,ui)else{return;};let already=self.active_identity.is_some();self.active_identity=Some(identity.into());if already{return;}self.frame_index=0;self.publish(ui);return;}
        let restore=self.active_identity.take().or_else(||identity.map(str::to_owned));self.frame_index=0;
        if let (Some(identity),Some(ui))=(restore,ui){self.last_fingerprint=None;self.recompute(&identity,ui,probe);}
    }
    fn publish(&self,ui:&mut dyn MemoryFooterUi){if !self.disposed&&let Some(identity)=&self.active_identity{ui.set_status(crate::status::MEMORY_STATUS_KEY,&format!("mem:{identity} {} reflecting",MEMORY_REFLECTING_FRAMES[self.frame_index]));}}
    pub fn tick(&mut self,ui:&mut dyn MemoryFooterUi){if self.disposed||self.active_identity.is_none(){return;}self.frame_index=(self.frame_index+1)%MEMORY_REFLECTING_FRAMES.len();self.publish(ui);}
    fn recompute(&mut self,identity:&str,ui:&mut dyn MemoryFooterUi,probe:&mut dyn MemoryFooterProbe){let Ok(fingerprint)=probe.fingerprint(identity)else{return;};if fingerprint.is_some()&&fingerprint==self.last_fingerprint{return;}let Ok(text)=probe.refresh_segments(identity)else{return;};self.last_fingerprint=fingerprint;if !self.disposed&&self.active_identity.is_none()&&let Some(text)=text{ui.set_status(crate::status::MEMORY_STATUS_KEY,&text);}}
    pub fn refresh(&mut self,identity:Option<&str>,ui:Option<&mut dyn MemoryFooterUi>,probe:&mut dyn MemoryFooterProbe){
        if self.disposed||self.active_identity.is_some(){return;}
        if let (Some(identity),Some(ui))=(identity,ui){self.recompute(identity,ui,probe);}
    }
    pub fn stop(&mut self){self.active_identity=None;self.frame_index=0;self.last_fingerprint=None;}
    pub fn dispose(&mut self){self.disposed=true;self.stop();}
    pub fn is_animating(&self)->bool{!self.disposed&&self.active_identity.is_some()}
}
#[cfg(test)]mod tests{
    use super::*;
    #[derive(Default)]struct Ui(Vec<String>);impl MemoryFooterUi for Ui{fn set_status(&mut self,key:&str,text:&str){assert_eq!(key,"memory");self.0.push(text.into());}}
    #[derive(Default)]struct Probe{fingerprint:Option<String>,calls:usize,fail:bool}impl MemoryFooterProbe for Probe{fn fingerprint(&mut self,_:&str)->Result<Option<String>,String>{if self.fail{Err("git".into())}else{Ok(self.fingerprint.clone())}}fn refresh_segments(&mut self,id:&str)->Result<Option<String>,String>{self.calls+=1;Ok(Some(format!("mem:{id} 2h ago* (+4)")))}}
    #[test]fn frames_wrap_and_settle_restores(){let mut live=MemoryFooterLive::default();let mut ui=Ui::default();let mut probe=Probe::default();live.set_active(Some("agent"),true,Some(&mut ui),&mut probe);for _ in 0..10{live.tick(&mut ui);}assert_eq!(&ui.0[..3],["mem:agent ⠋ reflecting","mem:agent ⠙ reflecting","mem:agent ⠹ reflecting"]);assert_eq!(ui.0[10],ui.0[0]);assert!(live.is_animating());live.set_active(Some("agent"),false,Some(&mut ui),&mut probe);assert!(!live.is_animating());assert_eq!(ui.0.last().unwrap(),"mem:agent 2h ago* (+4)");}
    #[test]fn stable_probe_skips_and_restore_bypasses_dedupe(){let mut live=MemoryFooterLive::default();let mut ui=Ui::default();let mut probe=Probe{fingerprint:Some("sha".into()),..Default::default()};for _ in 0..2{live.refresh(Some("agent"),Some(&mut ui),&mut probe);}assert_eq!(probe.calls,1);live.set_active(Some("agent"),true,Some(&mut ui),&mut probe);live.refresh(Some("agent"),Some(&mut ui),&mut probe);assert_eq!(probe.calls,1);live.set_active(Some("agent"),false,Some(&mut ui),&mut probe);assert_eq!(probe.calls,2);}
    #[test]fn missing_probe_always_refreshes_and_errors_are_silent(){let mut live=MemoryFooterLive::default();let mut ui=Ui::default();let mut probe=Probe::default();for _ in 0..2{live.refresh(Some("agent"),Some(&mut ui),&mut probe);}assert_eq!(probe.calls,2);probe.fail=true;live.refresh(Some("agent"),Some(&mut ui),&mut probe);assert_eq!(ui.0.len(),2);}
    #[test]fn stop_allows_restart_dispose_is_terminal(){let mut live=MemoryFooterLive::default();let mut ui=Ui::default();let mut probe=Probe::default();live.set_active(Some("agent"),true,Some(&mut ui),&mut probe);live.stop();live.tick(&mut ui);assert_eq!(ui.0.len(),1);live.set_active(Some("agent"),true,Some(&mut ui),&mut probe);assert_eq!(ui.0.len(),2);live.dispose();live.tick(&mut ui);live.set_active(Some("agent"),true,Some(&mut ui),&mut probe);live.refresh(Some("agent"),Some(&mut ui),&mut probe);assert_eq!(ui.0.len(),2);}
}
