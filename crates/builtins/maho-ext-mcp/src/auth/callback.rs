use std::{sync::Arc,time::Duration};
use axum::{Router,extract::State,http::{StatusCode,Uri},response::Html,routing::any};
use tokio::{net::TcpListener,sync::watch,task::JoinHandle};
use super::oauth_errors::{OAuthFailureKind,OAuthFlowError};

const SUCCESS_HTML:&str="<!doctype html><title>senpi</title><p>Authorization complete. You can close this tab.</p>";
const FAILURE_HTML:&str="<!doctype html><title>senpi</title><p>Authorization failed. Return to senpi and retry.</p>";
pub type StateValidator=Arc<dyn Fn(Option<&str>)->bool+Send+Sync>;
pub struct CallbackServerOptions {
    pub server_name:String,pub port:Option<u16>,pub host:Option<String>,pub path:Option<String>,
    pub timeout:Option<Duration>,pub validate_state:StateValidator,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct CallbackResult {pub code:String,pub state:Option<String>}
type Completion=Option<Result<CallbackResult,OAuthFlowError>>;
#[derive(Clone)]
struct HandlerState {name:String,path:String,validate:StateValidator,result:watch::Sender<Completion>}
pub struct CallbackChannel {
    pub redirect_url:String,pub uses_loopback:bool,
    result:watch::Receiver<Completion>,task:Option<JoinHandle<()>>,server_name:String,
}
impl CallbackChannel {
    pub async fn wait_for_code(&mut self)->Result<CallbackResult,OAuthFlowError> {
        if !self.uses_loopback {return Err(OAuthFlowError::new(OAuthFailureKind::Headless,format!("MCP server {} uses a callback URL override; complete with /mcp auth-complete <redirect-url>.",self.server_name)));}
        loop {
            let result=self.result.borrow().clone();
            if let Some(result)=result {self.close().await;return result;}
            if self.result.changed().await.is_err(){return Err(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,"callback server closed"));}
        }
    }
    pub async fn close(&mut self) {if let Some(task)=self.task.take(){task.abort();let _=task.await;}}
}
impl Drop for CallbackChannel {fn drop(&mut self){if let Some(task)=&self.task {task.abort();}}}
pub async fn open_callback_channel(options:CallbackServerOptions,override_url:Option<&str>)->Result<CallbackChannel,OAuthFlowError> {
    let (sender,receiver)=watch::channel(None);
    if let Some(url)=override_url.filter(|url|!url.is_empty()) {return Ok(CallbackChannel {redirect_url:url.into(),uses_loopback:false,result:receiver,task:None,server_name:options.server_name});}
    let host=options.host.as_deref().unwrap_or("127.0.0.1");let port=options.port.unwrap_or(0);
    let listener=TcpListener::bind((host,port)).await.map_err(|error| {
        let detail=if error.kind()==std::io::ErrorKind::AddrInUse {format!("port {port} is already in use (another senpi auth flow may be running); free it or configure a different callback.")}else{error.to_string()};
        OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} callback listener failed: {detail}",options.server_name))
    })?;
    let address=listener.local_addr().map_err(|e|OAuthFlowError::new(OAuthFailureKind::NeedsAuth,e.to_string()))?;
    let path=options.path.unwrap_or_else(||"/callback".into());let redirect_url=format!("http://{host}:{}{path}",address.port());
    let state=HandlerState {name:options.server_name.clone(),path,validate:options.validate_state,result:sender.clone()};
    let app=Router::new().fallback(any(handle)).with_state(state);
    let mut completion=receiver.clone();let name=options.server_name.clone();
    let task=tokio::spawn(async move {
        let stop=async move {let _=completion.changed().await;};
        let server=axum::serve(listener,app).with_graceful_shutdown(stop);
        tokio::select! {
            result=server=>{if let Err(error)=result {sender.send_replace(Some(Err(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,error.to_string()))));}}
            ()=tokio::time::sleep(options.timeout.unwrap_or(Duration::from_secs(300)))=>{sender.send_replace(Some(Err(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {name} authorization timed out after 5 minutes.")))));}
        }
    });
    Ok(CallbackChannel {redirect_url,uses_loopback:true,result:receiver,task:Some(task),server_name:options.server_name})
}
async fn handle(State(state):State<HandlerState>,uri:Uri)->(StatusCode,Html<&'static str>) {
    if uri.path()!=state.path {return (StatusCode::NOT_FOUND,Html(""));}
    let query=url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes());
    let mut code=None;let mut csrf=None;let mut error=None;
    for (key,value) in query {match key.as_ref(){"code" if code.is_none()=>code=Some(value.into_owned()),"state" if csrf.is_none()=>csrf=Some(value.into_owned()),"error" if error.is_none()=>error=Some(value.into_owned()),_=>()}}
    let valid=error.is_none() && code.is_some() && csrf.is_some() && (state.validate)(csrf.as_deref());
    let result=if valid {Ok(CallbackResult {code:code.unwrap_or_default(),state:csrf})}else{Err(OAuthFlowError::new(OAuthFailureKind::StateMismatch,format!("MCP server {} authorization state did not match (possible CSRF or a stale/replayed link); restart the auth flow.",state.name)))};
    state.result.send_if_modified(|current|{if current.is_some(){false}else{*current=Some(result);true}});
    if valid{(StatusCode::OK,Html(SUCCESS_HTML))}else{(StatusCode::BAD_REQUEST,Html(FAILURE_HTML))}
}
