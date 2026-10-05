use std::{future::Future,pin::Pin,sync::{Arc,Mutex},time::Duration};
use serde_json::{Value,json};
use tokio::task::JoinHandle;
use futures::{FutureExt,StreamExt};
use crate::{errors::McpError,log::{McpLogger,redact_mcp_log_text}};
pub type ErrorNotify=Arc<dyn Fn(String,McpError)->Pin<Box<dyn Future<Output=Result<(),McpError>>+Send>>+Send+Sync>;
pub type ErrorLogger=Arc<dyn Fn(&str,&Value)->Result<(),String>+Send+Sync>;
#[derive(Clone)]
pub struct McpAsyncErrorSink {pub logger:ErrorLogger,pub notify:Option<ErrorNotify>}
impl McpAsyncErrorSink {
    pub fn from_logger(logger:Arc<Mutex<McpLogger>>)->Self {
        Self {logger:Arc::new(move|scope,data|logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("error",scope,Some(data),None).map_err(|e|e.to_string())),notify:None}
    }
}
pub async fn wrap_async(scope:&str,callback:impl Future<Output=Result<(),McpError>>,sink:&McpAsyncErrorSink) {
    let result=std::panic::AssertUnwindSafe(callback).catch_unwind().await;
    match result {
        Ok(Ok(()))=>(),
        Ok(Err(error))=>report_mcp_async_error(scope,error,sink).await,
        Err(payload)=>report_mcp_async_error(scope,panic_error(payload),sink).await,
    }
}
fn panic_error(payload:Box<dyn std::any::Any+Send>)->McpError {
    let message=payload.downcast_ref::<String>().cloned().or_else(||payload.downcast_ref::<&str>().map(|message|(*message).to_owned())).unwrap_or_else(||"Non-string panic".into());
    McpError::new(crate::errors::McpErrorKind::Protocol,message)
}
pub async fn report_mcp_async_error(scope:&str,error:McpError,sink:&McpAsyncErrorSink) {
    log_error(scope,&error,sink);
    if let Some(notify)=&sink.notify {
        let result=std::panic::AssertUnwindSafe(async {notify(format!("MCP {scope} failed: {}",error.message),error).await}).catch_unwind().await;
        match result {Ok(Ok(()))=>(),Ok(Err(error))=>log_error(&format!("{scope}.notify"),&error,sink),Err(payload)=>log_error(&format!("{scope}.notify"),&panic_error(payload),sink)}
    }
}
fn log_error(scope:&str,error:&McpError,sink:&McpAsyncErrorSink) {
    let data=json!({"name":"Error","message":error.message});
    if !matches!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(||(sink.logger)(scope,&data))),Ok(Ok(()))) {
        match redact_mcp_log_text(scope) {
            Ok(scope)=>eprintln!("MCP {scope} logger failed; suppressed async error details"),
            Err(_)=>eprintln!("MCP logger failed; suppressed async error details"),
        }
    }
}
pub fn safe_timer<F,Fut>(scope:String,delay:Duration,callback:F,sink:McpAsyncErrorSink)->JoinHandle<()>
where F:FnOnce()->Fut+Send+'static,Fut:Future<Output=Result<(),McpError>>+Send+'static {
    let deadline=tokio::time::Instant::now()+delay;
    tokio::spawn(async move {tokio::time::sleep_until(deadline).await;wrap_async(&scope,callback(),&sink).await;})
}
pub fn safe_interval<F,Fut>(scope:String,delay:Duration,mut callback:F,sink:McpAsyncErrorSink)->JoinHandle<()>
where F:FnMut()->Fut+Send+'static,Fut:Future<Output=Result<(),McpError>>+Send+'static {
    let start=tokio::time::Instant::now()+delay.max(Duration::from_millis(1));
    tokio::spawn(async move {
        let mut interval=tokio::time::interval_at(start,delay.max(Duration::from_millis(1)));interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {interval.tick().await;wrap_async(&scope,callback(),&sink).await;}
    })
}
pub async fn safe_delay(delay:Duration) {tokio::time::sleep(delay).await;}

pub fn safe_on<T,F,Fut>(mut events:tokio::sync::broadcast::Receiver<T>,scope:String,mut listener:F,sink:McpAsyncErrorSink)->JoinHandle<()>
where T:Clone+Send+'static,F:FnMut(T)->Fut+Send+'static,Fut:Future<Output=Result<(),McpError>>+Send+'static {
    tokio::spawn(async move {
        let mut callbacks=futures::stream::FuturesUnordered::new();
        let mut closed=false;
        loop {
            tokio::select! {
                event=events.recv(),if !closed=>match event {
                    Ok(event)=>{
                        let callback=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||listener(event)));
                        let scope=scope.clone();let sink=sink.clone();
                        callbacks.push(async move {
                            match callback {
                                Ok(callback)=>wrap_async(&scope,callback,&sink).await,
                                Err(payload)=>report_mcp_async_error(&scope,panic_error(payload),&sink).await,
                            }
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>(),
                    Err(tokio::sync::broadcast::error::RecvError::Closed)=>closed=true,
                },
                _=callbacks.next(),if !callbacks.is_empty()=>(),
                else=>return,
                }
        }
    })
}
