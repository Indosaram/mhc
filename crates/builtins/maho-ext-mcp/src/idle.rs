use std::{sync::{Arc,Mutex},time::Duration,future::Future};
use tokio::{task::JoinHandle,sync::Notify};
use crate::{connection::{ServerConnection,ServerConnectionState},config_schema::{McpServerConfig,Lifecycle},errors::McpError};
pub const MCP_KEEP_ALIVE_INTERVAL_MS:u64=30000;
struct LifecycleState {in_flight:usize,idle:Option<JoinHandle<()>>,keep_alive:Option<JoinHandle<()>>,disposed:bool}
pub struct McpConnectionLifecycle {connection:Arc<ServerConnection>,config:McpServerConfig,state:Mutex<LifecycleState>,subscription:Mutex<Option<JoinHandle<()>>>,changed:Notify}
impl McpConnectionLifecycle {
    pub fn configure(connection:Arc<ServerConnection>,config:McpServerConfig)->Arc<Self> {
        let mut changes=connection.on_state_change();
        let lifecycle=Arc::new(Self {connection,config,state:Mutex::new(LifecycleState {in_flight:0,idle:None,keep_alive:None,disposed:false}),subscription:Mutex::new(None),changed:Notify::new()});
        let weak=Arc::downgrade(&lifecycle);
        let task=tokio::spawn(async move {while changes.recv().await.is_ok(){let Some(lifecycle)=weak.upgrade() else{return;};lifecycle.refresh();}});
        *lifecycle.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(task);lifecycle.refresh();lifecycle
    }
    fn refresh(self:&Arc<Self>) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.disposed{return;}
        if self.config.lifecycle==Some(Lifecycle::KeepAlive) {
            if let Some(timer)=state.idle.take(){timer.abort();}
            if state.keep_alive.is_none() {
                let weak=Arc::downgrade(self);let start=tokio::time::Instant::now()+Duration::from_millis(MCP_KEEP_ALIVE_INTERVAL_MS);
                state.keep_alive=Some(tokio::spawn(async move {
                    let mut interval=tokio::time::interval_at(start,Duration::from_millis(MCP_KEEP_ALIVE_INTERVAL_MS));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {interval.tick().await;let Some(lifecycle)=weak.upgrade() else{return;};
                        let connection=lifecycle.connection.clone();
                        let result=lifecycle.run_call(async {
                            match connection.state() {
                                ServerConnectionState::Connected=>{connection.client()?.request("ping",serde_json::json!({}),Duration::from_secs(2)).await?;}
                                ServerConnectionState::Idle|ServerConnectionState::Connecting=>{connection.connect().await?;}
                                _=>{connection.renew().await?;}
                            }Ok::<_,McpError>(())
                        }).await;
                        if let Err(error)=result {connection.mark_failure(ServerConnectionState::Degraded,Some(error));}
                    }
                }));
            }
            return;
        }
        if let Some(timer)=state.keep_alive.take(){timer.abort();}
        if self.connection.state()!=ServerConnectionState::Connected || state.in_flight>0 {if let Some(timer)=state.idle.take(){timer.abort();}return;}
        if state.idle.is_none() {
            let weak=Arc::downgrade(self);let deadline=tokio::time::Instant::now()+Duration::from_secs_f64(self.config.idle_timeout_min.unwrap_or(10.0)*60.0);
            state.idle=Some(tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;let Some(lifecycle)=weak.upgrade() else{return;};
                let idle={let mut state=lifecycle.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.idle=None;state.in_flight==0 && !state.disposed};
                if idle && lifecycle.connection.state()==ServerConnectionState::Connected && let Err(error)=lifecycle.connection.bump_generation().await {lifecycle.connection.mark_failure(ServerConnectionState::Degraded,Some(error));}
            }));
        }
    }
    pub async fn run_call<T>(self:&Arc<Self>,call:impl Future<Output=T>)->T {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.in_flight+=1;if let Some(timer)=state.idle.take(){timer.abort();}}
        struct InFlight(Arc<McpConnectionLifecycle>);
        impl Drop for InFlight {fn drop(&mut self) {{let mut state=self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.in_flight-=1;}self.0.refresh();self.0.changed.notify_waiters();}}
        let _in_flight=InFlight(self.clone());call.await
    }
    pub fn in_flight(&self)->usize {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).in_flight}
    pub fn dispose(&self) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.disposed=true;
        if let Some(timer)=state.idle.take(){timer.abort();}
        if let Some(timer)=state.keep_alive.take(){timer.abort();}
        if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
    }
}
impl Drop for McpConnectionLifecycle {fn drop(&mut self){self.dispose();}}
