use std::{future::Future,pin::Pin,sync::{Arc,Mutex},time::Duration};
use tokio::{task::JoinHandle,time::Instant};
use crate::{connection::{ServerConnection,ServerConnectionState},errors::{McpError,McpErrorKind}};
pub const MCP_RECONNECT_BACKOFF_MS:[u64;5]=[500,1000,2000,4000,8000];
pub const MCP_RECONNECT_BREAKER_WINDOW_MS:u64=30000;
pub const MCP_RECONNECT_BREAKER_ATTEMPTS:usize=5;
pub type ReconnectCallback=Arc<dyn Fn()->Pin<Box<dyn Future<Output=Result<(),McpError>>+Send>>+Send+Sync>;
struct ReconnectState {attempts:Vec<Instant>,backoff:usize,timer:Option<JoinHandle<()>>,disposed:bool}
pub struct McpReconnect {
    connection:Arc<ServerConnection>,state:Mutex<ReconnectState>,callback:ReconnectCallback,
    should_reconnect:Arc<dyn Fn()->bool+Send+Sync>,random:Arc<dyn Fn()->f64+Send+Sync>,subscription:Mutex<Option<JoinHandle<()>>>,
}
impl McpReconnect {
    pub fn configure(connection:Arc<ServerConnection>,callback:ReconnectCallback,should_reconnect:Arc<dyn Fn()->bool+Send+Sync>,random:Arc<dyn Fn()->f64+Send+Sync>)->Arc<Self> {
        let mut changes=connection.on_state_change();
        let reconnect=Arc::new(Self {connection,state:Mutex::new(ReconnectState {attempts:Vec::new(),backoff:0,timer:None,disposed:false}),callback,should_reconnect,random,subscription:Mutex::new(None)});
        let weak=Arc::downgrade(&reconnect);
        let task=tokio::spawn(async move {while let Ok(event)=changes.recv().await {let Some(reconnect)=weak.upgrade() else{return;};match event.state {ServerConnectionState::Degraded=>reconnect.schedule(event.generation),ServerConnectionState::Connected=>reconnect.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).backoff=0,_=>()}}});
        *reconnect.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(task);reconnect
    }
    fn prune(state:&mut ReconnectState){state.attempts.retain(|at|at.elapsed()<=Duration::from_millis(MCP_RECONNECT_BREAKER_WINDOW_MS));}
    fn open_circuit(&self) {
        let mut error=McpError::new(McpErrorKind::Connect,format!("MCP server {} reconnect circuit breaker opened after {MCP_RECONNECT_BREAKER_ATTEMPTS} attempts in 30s; run /mcp reconnect {}",self.connection.server_name,self.connection.server_name));
        error.phase=Some("reconnect".into());error.server_name=Some(self.connection.server_name.clone());
        self.connection.mark_failure(ServerConnectionState::Suspended,Some(error));
    }
    fn schedule(self:&Arc<Self>,generation:u64) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed || state.timer.is_some() || !(self.should_reconnect)() || matches!(self.connection.state(),ServerConnectionState::Disabled|ServerConnectionState::Suspended){return;}
        Self::prune(&mut state);if state.attempts.len()>=MCP_RECONNECT_BREAKER_ATTEMPTS {drop(state);self.open_circuit();return;}
        let random=(self.random)();let random=if random.is_finite(){random.clamp(0.0,1.0)}else{1.0};
        let delay=Duration::from_secs_f64(((MCP_RECONNECT_BACKOFF_MS[state.backoff.min(4)] as f64)*random).floor()/1000.0);
        let deadline=Instant::now()+delay;let weak=Arc::downgrade(self);
        state.timer=Some(tokio::spawn(async move {tokio::time::sleep_until(deadline).await;if let Some(reconnect)=weak.upgrade(){reconnect.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).timer=None;reconnect.attempt(generation,false).await;}}));
    }
    async fn attempt(self:&Arc<Self>,generation:u64,manual:bool)->Option<McpError> {
        {
            let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.disposed || !(self.should_reconnect)() || (!manual && generation!=self.connection.generation()) || self.connection.state()==ServerConnectionState::Disabled {return None;}
            Self::prune(&mut state);if !manual && state.attempts.len()>=MCP_RECONNECT_BREAKER_ATTEMPTS {drop(state);self.open_circuit();return None;}
            state.attempts.push(Instant::now());state.backoff+=1;
        }
        match (self.callback)().await {
            Ok(())=>{self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).backoff=0;None}
            Err(error)=>{
                if !matches!(self.connection.state(),ServerConnectionState::Suspended|ServerConnectionState::NeedsAuth) {
                    let visible=if error.message.contains("connect was superseded") || error.message.contains("transport closed"){self.connection.last_error().unwrap_or_else(||error.clone())}else{error.clone()};
                    self.connection.mark_failure(ServerConnectionState::Degraded,Some(visible));self.schedule(self.connection.generation());
                }
                if manual{Some(error)}else{None}
            }
        }
    }
    pub async fn reconnect_now(self:&Arc<Self>)->Result<(),McpError> {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(timer)=state.timer.take(){timer.abort();}state.attempts.clear();state.backoff=0;}
        match self.attempt(self.connection.generation(),true).await {Some(error)=>Err(error),None=>Ok(())}
    }
    pub fn dispose(&self) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.disposed=true;if let Some(timer)=state.timer.take(){timer.abort();}
        if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
    }
    pub fn attempts_in_window(&self)->usize {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);Self::prune(&mut state);state.attempts.len()}
}
impl Drop for McpReconnect {fn drop(&mut self){self.dispose();}}
