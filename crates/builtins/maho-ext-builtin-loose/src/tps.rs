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
