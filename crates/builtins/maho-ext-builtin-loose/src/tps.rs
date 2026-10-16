#[derive(Default)]
pub struct Timing{active_start_ms:Option<f64>,elapsed_ms:f64,message_start_ms:Option<f64>,first_delta_ms:Option<f64>,last_delta_ms:Option<f64>,thinking_streamed:bool,generation_ms:f64,streamed_output:f64}
#[derive(Debug,PartialEq)]
pub struct Statistics{pub tokens_per_second:f64,pub cache_hit_rate:f64,pub elapsed_seconds:f64}
#[derive(Debug,PartialEq)]
pub struct Notice{pub text:String,pub statistics:Statistics}
#[derive(Clone,Copy)]
pub struct TurnUsage{pub input:f64,pub output:f64,pub cache_read:f64,pub cache_write:f64}
impl Timing{
    pub fn reset(&mut self){self.active_start_ms=None;self.elapsed_ms=0.0;self.reset_stream_window();self.generation_ms=0.0;self.streamed_output=0.0;}
    fn reset_stream_window(&mut self){self.message_start_ms=None;self.first_delta_ms=None;self.last_delta_ms=None;self.thinking_streamed=false;}
    pub fn finish_message(&mut self,monotonic_ms:f64){if let Some(start)=self.active_start_ms.take(){let elapsed=monotonic_ms-start;if elapsed>0.0{self.elapsed_ms+=elapsed;}}}
    pub fn start_message(&mut self,monotonic_ms:f64){self.finish_message(monotonic_ms);self.reset_stream_window();self.active_start_ms=Some(monotonic_ms);self.message_start_ms=Some(monotonic_ms);}
    pub fn record_delta(&mut self,monotonic_ms:f64,kind:&str){if !matches!(kind,"text_delta"|"thinking_delta"|"toolcall_delta"){return;}
        if kind=="thinking_delta"{self.thinking_streamed=true;}self.first_delta_ms.get_or_insert(monotonic_ms);self.last_delta_ms=Some(monotonic_ms);}
    pub fn finish_streamed_message(&mut self,monotonic_ms:f64,output:f64,reasoning:f64,has_thinking_block:bool,stop_reason:&str){
        self.finish_message(monotonic_ms);
        let hidden_reasoning=reasoning>0.0||(!self.thinking_streamed&&has_thinking_block);
        let start=if hidden_reasoning{self.message_start_ms}else{self.first_delta_ms};
        if !matches!(stop_reason,"aborted"|"error")&&output>0.0&&let(Some(start),Some(end))=(start,self.last_delta_ms)&&end>start{self.generation_ms+=end-start;self.streamed_output+=output;}
        self.reset_stream_window();
    }
    pub fn finish_turn(&mut self,monotonic_ms:f64,has_ui:bool,input:f64,output:f64,cache_read:f64,cache_write:f64)->Option<Statistics>{
        self.finish_message(monotonic_ms);let elapsed=self.elapsed_ms;self.reset();if !has_ui||elapsed<=0.0||output<=0.0{return None;}
        let prompt=input+cache_read+cache_write;Some(Statistics{tokens_per_second:output/(elapsed/1000.0),cache_hit_rate:if prompt>0.0{cache_read/prompt*100.0}else{0.0},elapsed_seconds:elapsed/1000.0})
    }
    pub fn finish_turn_notice(&mut self,monotonic_ms:f64,has_ui:bool,is_tui:bool,usage:TurnUsage)->Option<Notice>{
        let TurnUsage{input,output,cache_read,cache_write}=usage;
        self.finish_message(monotonic_ms);let elapsed=self.elapsed_ms;let generation=self.generation_ms;let streamed=self.streamed_output;self.reset();if !has_ui||!is_tui||elapsed<=0.0||output<=0.0{return None;}
        let prompt=input+cache_read+cache_write;let rate=if generation>=500.0&&streamed>0.0{format!("TPS {:.1} tok/s. ",streamed/(generation/1000.0))}else{String::new()};
        let statistics=Statistics{tokens_per_second:if generation>0.0{streamed/(generation/1000.0)}else{0.0},cache_hit_rate:if prompt>0.0{cache_read/prompt*100.0}else{0.0},elapsed_seconds:elapsed/1000.0};
        Some(Notice{text:format!("{rate}Cache hit {:.1}%, {:.1}s",statistics.cache_hit_rate,statistics.elapsed_seconds),statistics})
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
        for kind in [EventKind::AgentStart,EventKind::MessageStart,EventKind::MessageUpdate,EventKind::MessageEnd,EventKind::AgentEnd]{
            let state=state.clone();api.on(kind,Arc::new(move|event,ctx|{let state=state.clone();Box::pin(async move{
                let now=origin.elapsed().as_secs_f64()*1000.0;let mut timing=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                match event{
                    ExtensionEvent::AgentStart=>timing.reset(),
                    ExtensionEvent::MessageStart{message:AgentMessage::Llm(Message::Assistant(_))}=>timing.start_message(now),
                    ExtensionEvent::MessageUpdate{message:AgentMessage::Llm(Message::Assistant(_)),assistant_message_event}=>timing.record_delta(now,assistant_message_event.get("type").and_then(serde_json::Value::as_str).unwrap_or("other")),
                    ExtensionEvent::MessageEnd{message:AgentMessage::Llm(Message::Assistant(message))}=>timing.finish_streamed_message(now,message.usage.output as f64,message.usage.reasoning.unwrap_or_default() as f64,message.content.iter().any(|block|matches!(block,maho_ai::types::ContentBlock::Thinking(_))),match message.stop_reason{maho_ai::types::StopReason::Aborted=>"aborted",maho_ai::types::StopReason::Error=>"error",_=>"other"}),
                    ExtensionEvent::AgentEnd{messages,..}=>{
                        let mut input=0.0;let mut output=0.0;let mut read=0.0;let mut write=0.0;
                        for message in messages{if let AgentMessage::Llm(Message::Assistant(message))=message{input+=message.usage.input as f64;output+=message.usage.output as f64;read+=message.usage.cache_read as f64;write+=message.usage.cache_write as f64;}}
                        let notice=timing.finish_turn_notice(now,ctx.has_ui,ctx.mode==maho_ext_api::ExtensionMode::Tui,TurnUsage{input,output,cache_read:read,cache_write:write});drop(timing);
                        if let Some(notice)=notice{ctx.ui.notify(&notice.text,NotificationType::Info);}
                    }
                    _=>{}
                }
                Ok(EventResult::None)
            })}));
        }
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn measures_stream_interval_suppresses_short_rates_and_non_tui_notices(){
        let mut timing=Timing::default();timing.start_message(0.0);timing.record_delta(100.0,"text_delta");timing.record_delta(800.0,"text_delta");timing.finish_streamed_message(900.0,70.0,0.0,false,"stop");
        let notice=timing.finish_turn_notice(1_000.0,true,true,TurnUsage{input:20.0,output:70.0,cache_read:10.0,cache_write:0.0}).expect("TUI notice");
        assert_eq!(notice.statistics.tokens_per_second,100.0);assert!(notice.text.starts_with("TPS 100.0 tok/s."));
        let mut timing=Timing::default();timing.start_message(0.0);timing.record_delta(100.0,"text_delta");timing.record_delta(300.0,"text_delta");timing.finish_streamed_message(400.0,70.0,0.0,false,"stop");
        assert!(!timing.finish_turn_notice(500.0,true,true,TurnUsage{input:20.0,output:70.0,cache_read:0.0,cache_write:0.0}).expect("stats notice").text.starts_with("TPS"));
        assert!(timing.finish_turn_notice(500.0,true,false,TurnUsage{input:20.0,output:70.0,cache_read:0.0,cache_write:0.0}).is_none());
    }
    #[test]
    fn excludes_failed_stops_and_uses_message_start_for_hidden_reasoning(){
        let mut timing=Timing::default();timing.start_message(0.0);timing.record_delta(200.0,"text_delta");timing.record_delta(400.0,"text_delta");timing.finish_streamed_message(500.0,40.0,10.0,false,"error");
        assert_eq!(timing.streamed_output,0.0);
        timing.start_message(1_000.0);timing.record_delta(1_500.0,"text_delta");timing.record_delta(2_000.0,"text_delta");timing.finish_streamed_message(2_100.0,50.0,5.0,false,"stop");
        assert_eq!(timing.generation_ms,1_000.0);
    }
}
