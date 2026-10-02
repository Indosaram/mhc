use serde_json::Value;
use super::types::TokenUsageSnapshot;
pub fn empty_usage()->TokenUsageSnapshot{TokenUsageSnapshot::default()}
pub fn collect_assistant_usage(messages:&[Value])->TokenUsageSnapshot{let mut usage=empty_usage();for message in messages{add(&mut usage,message);}usage}
#[derive(Default)]
pub struct TurnUsageTracker{pending:TokenUsageSnapshot,flushed:TokenUsageSnapshot}
impl TurnUsageTracker{
    pub fn reset(&mut self){self.pending=empty_usage();self.flushed=empty_usage();}
    pub fn note_message_end(&mut self,message:&Value){add(&mut self.pending,message);}
    pub fn take_pending(&mut self)->TokenUsageSnapshot{let taken=std::mem::take(&mut self.pending);self.flushed.input+=taken.input;self.flushed.output+=taken.output;self.flushed.cache_read+=taken.cache_read;self.flushed.cache_write+=taken.cache_write;self.flushed.total_tokens+=taken.total_tokens;taken}
    pub fn discard_pending(&mut self){self.take_pending();}
    pub fn take_remaining(&mut self,messages:&[Value])->TokenUsageSnapshot{
        let collected=collect_assistant_usage(messages);
        let mut remaining=empty_usage();
        for (collected,flushed,remaining) in [(collected.input,&mut self.flushed.input,&mut remaining.input),(collected.output,&mut self.flushed.output,&mut remaining.output),(collected.cache_read,&mut self.flushed.cache_read,&mut remaining.cache_read),(collected.cache_write,&mut self.flushed.cache_write,&mut remaining.cache_write),(collected.total_tokens,&mut self.flushed.total_tokens,&mut remaining.total_tokens)]{
            let difference=collected-*flushed;
            *remaining=if difference.is_nan(){f64::NAN}else{difference.max(0.0)};*flushed=if flushed.is_nan()||collected.is_nan(){f64::NAN}else{flushed.max(collected)};
        }
        self.pending=empty_usage();remaining
    }
}
fn add(target:&mut TokenUsageSnapshot,message:&Value){
    if message.get("role").and_then(Value::as_str)!=Some("assistant"){return;}
    let Some(usage)=message.get("usage").and_then(Value::as_object)else{return;};
    for (field,target) in [("input",&mut target.input),("output",&mut target.output),("cacheRead",&mut target.cache_read),("cacheWrite",&mut target.cache_write),("totalTokens",&mut target.total_tokens)]{
        if let Some(value)=usage.get(field).and_then(Value::as_f64).filter(|v|v.is_finite()){*target+=value;}
    }
}
