#[derive(Default)]
pub struct Timing{active_start_ms:Option<f64>,elapsed_ms:f64}
#[derive(Debug,PartialEq)]
pub struct Statistics{pub tokens_per_second:f64,pub cache_hit_rate:f64,pub elapsed_seconds:f64}
impl Timing{
    pub fn reset(&mut self){self.active_start_ms=None;self.elapsed_ms=0.0;}
    pub fn finish_message(&mut self,monotonic_ms:f64){if let Some(start)=self.active_start_ms.take(){let elapsed=monotonic_ms-start;if elapsed>0.0{self.elapsed_ms+=elapsed;}}}
    pub fn start_message(&mut self,monotonic_ms:f64){self.finish_message(monotonic_ms);self.active_start_ms=Some(monotonic_ms);}
    pub fn finish_turn(&mut self,monotonic_ms:f64,has_ui:bool,input:f64,output:f64,cache_read:f64,cache_write:f64)->Option<Statistics>{
        self.finish_message(monotonic_ms);let elapsed=self.elapsed_ms;self.reset();if !has_ui||elapsed<=0.0||output<=0.0{return None;}
        let prompt=input+cache_read+cache_write;Some(Statistics{tokens_per_second:output/(elapsed/1000.0),cache_hit_rate:if prompt>0.0{cache_read/prompt*100.0}else{0.0},elapsed_seconds:elapsed/1000.0})
    }
}
pub struct Tps;
impl maho_ext_api::Extension for Tps{
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        use maho_ext_api::{EventKind,ExtensionEvent,EventResult,NotificationType};
        use maho_agent::types::AgentMessage;
        use maho_ai::types::Message;
        use std::sync::{Arc,Mutex};
        let state=Arc::new(Mutex::new(Timing::default()));let origin=std::time::Instant::now();
        for kind in [EventKind::AgentStart,EventKind::MessageStart,EventKind::MessageEnd,EventKind::AgentEnd]{
            let state=state.clone();api.on(kind,Arc::new(move|event,ctx|{let state=state.clone();Box::pin(async move{
                let now=origin.elapsed().as_secs_f64()*1000.0;let mut timing=state.lock().expect("TPS timing lock");
                match event{
                    ExtensionEvent::AgentStart=>timing.reset(),
                    ExtensionEvent::MessageStart{message:AgentMessage::Llm(Message::Assistant(_))}=>timing.start_message(now),
                    ExtensionEvent::MessageEnd{message:AgentMessage::Llm(Message::Assistant(_))}=>timing.finish_message(now),
                    ExtensionEvent::AgentEnd{messages,..}=>{
                        let mut input=0.0;let mut output=0.0;let mut read=0.0;let mut write=0.0;
                        for message in messages{if let AgentMessage::Llm(Message::Assistant(message))=message{input+=message.usage.input as f64;output+=message.usage.output as f64;read+=message.usage.cache_read as f64;write+=message.usage.cache_write as f64;}}
                        let statistics=timing.finish_turn(now,ctx.has_ui,input,output,read,write);drop(timing);
                        if let Some(stats)=statistics{ctx.ui.notify(&format!("TPS {:.1} tok/s. Cache hit {:.1}%, {:.1}s",stats.tokens_per_second,stats.cache_hit_rate,stats.elapsed_seconds),NotificationType::Info);}
                    }
                    _=>{}
                }
                Ok(EventResult::None)
            })}));
        }
    }
}
