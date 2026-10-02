use serde_json::Value;
pub const MCP_LOG_RATE_LIMIT_PER_SECOND:f64=10.0;
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum LoggerMethod {Debug,Info,Warn,Error}
fn severity(level:&str)->usize { ["emergency","alert","critical","error","warning","notice","info","debug"].iter().position(|item|*item==level).unwrap_or(6) }
pub struct McpServerLogging {threshold:Option<usize>,rate:f64,tokens:f64,last_refill:f64}
impl McpServerLogging {
    pub fn new(level:Option<&str>,rate:Option<f64>,now:f64)->Self {let rate=rate.unwrap_or(MCP_LOG_RATE_LIMIT_PER_SECOND);Self {threshold:level.map(severity),rate,tokens:rate,last_refill:now}}
    pub fn message(&mut self,level:&str,data:&Value,logger:Option<&str>,now:f64)->Option<(LoggerMethod,String)> {
        if self.threshold.is_some_and(|threshold|severity(level)>threshold){return None;}
        self.tokens=self.rate.min(self.tokens+(now-self.last_refill)/1000.0*self.rate);self.last_refill=now;
        if self.tokens<1.0{return None;}self.tokens-=1.0;
        let method=match level {"debug"=>LoggerMethod::Debug,"info"|"notice"=>LoggerMethod::Info,"warning"=>LoggerMethod::Warn,_=>LoggerMethod::Error};
        let text=data.as_str().map_or_else(||data.to_string(),str::to_owned);
        let name=logger.filter(|s|!s.is_empty()).map_or_else(String::new,|s|format!(":{s}"));
        Some((method,format!("[server{name}] {text}")))
    }
}
