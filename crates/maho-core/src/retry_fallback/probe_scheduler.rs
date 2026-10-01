use maho_ai::utils::abort::{AbortController,AbortSignal};

#[derive(Debug,Clone,PartialEq)]
pub enum ProbeBackEvent {
    Scheduled {selector:String,at_ms:f64,probe_index:u8},
    Result {selector:String,ok:bool,error_message:Option<String>},
}
pub struct ProbeBackPlan {pub selector:String,pub first_at_ms:f64,pub deadline_ms:f64}
pub struct ProbeTicket {pub generation:u64,pub index:u8,pub signal:AbortSignal}
#[derive(Default)]
pub struct ProbeBackScheduler {generation:u64,plan:Option<ProbeBackPlan>,active:Option<(u8,AbortController)>,first_started:bool,second_announced:bool}
impl ProbeBackScheduler {
    pub fn active(&self)->bool {self.plan.is_some()}
    pub fn arm(&mut self,plan:ProbeBackPlan)->Vec<ProbeBackEvent> {
        self.cancel();let event=ProbeBackEvent::Scheduled{selector:plan.selector.clone(),at_ms:plan.first_at_ms,probe_index:1};self.plan=Some(plan);vec![event]
    }
    pub fn cancel(&mut self){self.generation+=1;if let Some((_,controller))=self.active.take(){controller.abort(None);}self.plan=None;self.first_started=false;self.second_announced=false;}
    pub fn next_deadline(&self)->Option<f64>{self.plan.as_ref().map(|p|if self.first_started{p.deadline_ms}else{p.first_at_ms.min(p.deadline_ms)})}
    pub fn begin_due_probe(&mut self,now:f64,auth_available:bool)->(Option<ProbeTicket>,Vec<ProbeBackEvent>) {
        let Some(plan)=&self.plan else{return (None,Vec::new());};
        let index=if now>=plan.deadline_ms{2}else if now>=plan.first_at_ms&&!self.first_started{1}else{return (None,Vec::new());};
        if self.active.as_ref().is_some_and(|(i,_)|*i==index){return (None,Vec::new());}
        let mut events=Vec::new();
        if index==2 {
            if let Some((_,controller))=self.active.take(){controller.abort(None);}
            if !self.second_announced{self.second_announced=true;events.push(ProbeBackEvent::Scheduled{selector:plan.selector.clone(),at_ms:plan.deadline_ms,probe_index:2});}
        }
        if !auth_available {events.push(ProbeBackEvent::Result{selector:plan.selector.clone(),ok:false,error_message:Some("auth-unavailable".into())});self.cancel();return (None,events);}
        let controller=AbortController::new();let signal=controller.signal();self.active=Some((index,controller));self.first_started=true;
        (Some(ProbeTicket{generation:self.generation,index,signal}),events)
    }
    pub fn finish_probe(&mut self,ticket:&ProbeTicket,success:bool)->Vec<ProbeBackEvent> {
        if ticket.generation!=self.generation||self.active.as_ref().is_none_or(|(i,_)|*i!=ticket.index){return Vec::new();}
        self.active=None;let Some(plan)=&self.plan else{return Vec::new();};
        if success||ticket.index==2 {let event=ProbeBackEvent::Result{selector:plan.selector.clone(),ok:success,error_message:None};self.cancel();vec![event]}
        else if !self.second_announced {self.second_announced=true;vec![ProbeBackEvent::Scheduled{selector:plan.selector.clone(),at_ms:plan.deadline_ms,probe_index:2}]} else {Vec::new()}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_aborts_first_probe_and_stale_completion_is_ignored() {
        let mut scheduler=ProbeBackScheduler::default();scheduler.arm(ProbeBackPlan{selector:"p/m".into(),first_at_ms:50.0,deadline_ms:100.0});
        let first=scheduler.begin_due_probe(50.0,true).0.expect("first");
        let second=scheduler.begin_due_probe(100.0,true).0.expect("second");assert!(first.signal.aborted());
        assert!(scheduler.finish_probe(&first,true).is_empty());assert_eq!(scheduler.finish_probe(&second,true).len(),1);assert!(!scheduler.active());
    }
    #[test]
    fn cancellation_prevents_completion_events() {
        let mut scheduler=ProbeBackScheduler::default();scheduler.arm(ProbeBackPlan{selector:"p/m".into(),first_at_ms:0.0,deadline_ms:100.0});
        let ticket=scheduler.begin_due_probe(0.0,true).0.expect("first");scheduler.cancel();assert!(ticket.signal.aborted());assert!(scheduler.finish_probe(&ticket,true).is_empty());
    }
}
