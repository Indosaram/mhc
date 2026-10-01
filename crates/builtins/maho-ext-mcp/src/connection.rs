use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use tokio::sync::{broadcast,watch};
use crate::{config_schema::McpServerConfig,errors::{McpError,McpErrorKind},log::McpLogger,transport::{McpTransportConnection,create_mcp_transport,connect_mcp_transport,shutdown_mcp_transport},transport_sdk::McpClient};
pub use crate::connection_types::*;
type ConnectResult=Option<Result<Arc<McpClient>,McpError>>;
struct ConnectionInner {state:ServerConnectionState,generation:u64,last_error:Option<McpError>,transport:Option<Arc<McpTransportConnection>>,pending:Option<watch::Receiver<ConnectResult>>}
pub struct ServerConnection {
    pub server_name:String,config:McpServerConfig,env:Option<BTreeMap<String,String>>,logger:Arc<Mutex<McpLogger>>,
    inner:Mutex<ConnectionInner>,states:broadcast::Sender<ServerConnectionStateChangedEvent>,tools:broadcast::Sender<ServerConnectionToolsChangedEvent>,
}
impl ServerConnection {
    pub fn new(server:&str,config:McpServerConfig,env:Option<BTreeMap<String,String>>,logger:Arc<Mutex<McpLogger>>)->Arc<Self> {
        let (states,_)=broadcast::channel(256);let (tools,_)=broadcast::channel(256);
        let state=if config.enabled==Some(true){ServerConnectionState::Idle}else{ServerConnectionState::Disabled};
        Arc::new(Self {server_name:server.into(),config,env,logger,inner:Mutex::new(ConnectionInner {state,generation:0,last_error:None,transport:None,pending:None}),states,tools})
    }
    pub fn state(&self)->ServerConnectionState {self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state}
    pub fn generation(&self)->u64 {self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).generation}
    pub fn last_error(&self)->Option<McpError> {self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last_error.clone()}
    pub fn on_state_change(&self)->broadcast::Receiver<ServerConnectionStateChangedEvent> {self.states.subscribe()}
    pub fn on_tools_changed(&self)->broadcast::Receiver<ServerConnectionToolsChangedEvent> {self.tools.subscribe()}
    fn error(&self,message:String,phase:&str)->McpError {let mut error=McpError::new(McpErrorKind::Connect,message);error.phase=Some(phase.into());error.server_name=Some(self.server_name.clone());error}
    fn transition(&self,inner:&mut ConnectionInner,state:ServerConnectionState,error:Option<McpError>) {
        if inner.state==state{return;}let previous_state=inner.state;inner.state=state;
        let _=self.states.send(ServerConnectionStateChangedEvent {server_name:self.server_name.clone(),generation:inner.generation,state,previous_state,error});
    }
    pub fn mark_failure(&self,state:ServerConnectionState,error:Option<McpError>) {let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);inner.last_error=error.clone();self.transition(&mut inner,state,error);}
    pub fn mark_tools_changed(&self) {let _=self.tools.send(ServerConnectionToolsChangedEvent {server_name:self.server_name.clone(),generation:self.generation()});}
    pub fn client(&self)->Result<Arc<McpClient>,McpError> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).transport.as_ref().ok_or_else(||self.error(format!("MCP server {} is not connected",self.server_name),"client"))?.client()
    }
    pub fn get_root_pid(&self)->Option<u32> {self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).transport.as_ref().and_then(|transport|transport.get_root_pid())}
    pub async fn connect(self:&Arc<Self>)->Result<Arc<McpClient>,McpError> {
        let mut receiver={
            let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.state==ServerConnectionState::Disabled {return Err(self.error(format!("MCP server {} is disabled",self.server_name),"connect"));}
            if inner.state==ServerConnectionState::Connected {return inner.transport.as_ref().ok_or_else(||self.error("connected transport absent".into(),"client"))?.client();}
            if let Some(pending)=&inner.pending {pending.clone()}else{
                let (sender,receiver)=watch::channel(None);inner.pending=Some(receiver.clone());let generation=inner.generation;let connection=self.clone();
                tokio::spawn(async move {
                    let result=connection.open_connection(generation).await;
                    {let mut inner=connection.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if inner.generation==generation{inner.pending=None;}}
                    sender.send_replace(Some(result));
                });receiver
            }
        };
        loop {
            if let Some(result)=receiver.borrow().clone(){return result;}
            if receiver.changed().await.is_err(){return Err(self.error("connect task closed".into(),"connect"));}
        }
    }
    async fn open_connection(self:&Arc<Self>,generation:u64)->Result<Arc<McpClient>,McpError> {
        let transport=match create_mcp_transport(&self.server_name,&self.config,self.env.as_ref(),self.logger.clone()) {
            Ok(transport)=>Arc::new(transport),
            Err(error)=>{
                let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if inner.generation==generation && inner.state!=ServerConnectionState::Disabled {
                    inner.last_error=Some(error.clone());self.transition(&mut inner,ServerConnectionState::Degraded,Some(error.clone()));
                }
                return Err(error);
            }
        };
        {
            let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.generation!=generation || inner.state==ServerConnectionState::Disabled{return Err(self.error(format!("MCP server {} connect was superseded",self.server_name),"connect"));}
            inner.transport=Some(transport.clone());self.transition(&mut inner,ServerConnectionState::Connecting,None);
        }
        let result=match connect_mcp_transport(&transport).await {
            Ok(client)=>Ok(client),
            Err(error)=>Err(crate::diagnose::diagnose_mcp_connect_failure(&self.server_name,&self.config,self.env.as_ref(),&error,self.logger.clone()).await),
        };
        let current={
            let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.generation!=generation || inner.state==ServerConnectionState::Disabled {false}else{
                match &result {
                    Ok(_)=>{inner.last_error=None;self.transition(&mut inner,ServerConnectionState::Connected,None);}
                    Err(error)=>{inner.transport=None;inner.last_error=Some(error.clone());self.transition(&mut inner,ServerConnectionState::Degraded,Some(error.clone()));}
                }true
            }
        };
        if !current {let _=shutdown_mcp_transport(&transport).await;return Err(self.error(format!("MCP server {} connect was superseded",self.server_name),"connect"));}
        if let Ok(client)=&result {
            self.mark_tools_changed();self.subscribe_client(client,generation);
        }
        result
    }
    fn subscribe_client(self:&Arc<Self>,client:&Arc<McpClient>,generation:u64) {
        let mut notifications=client.notifications.subscribe();let mut closed=client.closed.subscribe();let weak=Arc::downgrade(self);
        let logger=self.logger.clone();let threshold=self.config.log_level.and_then(|level|serde_json::to_value(level).ok().and_then(|value|value.as_str().map(str::to_owned)));
        tokio::spawn(async move {
            let mut logging=crate::logging::McpServerLogging::new(threshold.as_deref(),None,0.0);let started=tokio::time::Instant::now();
            loop {
                let Some(connection)=weak.upgrade() else{return;};
                if connection.generation()!=generation || connection.state()==ServerConnectionState::Disabled{return;}
                if *closed.borrow() {
                    let error=connection.error(format!("MCP server {} transport closed",connection.server_name),"close");connection.mark_failure(ServerConnectionState::Degraded,Some(error));return;
                }
                drop(connection);
                tokio::select! {
                    result=closed.changed()=>{if result.is_err(){return;}}
                    result=notifications.recv()=>{
                        let value=match result {Ok(value)=>value,Err(broadcast::error::RecvError::Lagged(_))=>continue,Err(_)=>return};
                        let Some(connection)=weak.upgrade() else{return;};
                        if connection.generation()!=generation{return;}
                        match crate::notification_schemas::parse_notification(&value) {
                            Some(crate::notification_schemas::McpNotification::ListChanged {..}|crate::notification_schemas::McpNotification::ResourceUpdated {..})=>connection.mark_tools_changed(),
                            Some(crate::notification_schemas::McpNotification::LoggingMessage {level,logger:server_logger,data,..})=>{
                                if let Some((method,text))=logging.message(&level,&data.unwrap_or(serde_json::Value::Null),server_logger.as_deref(),started.elapsed().as_secs_f64()*1000.0) {
                                    let level=match method {crate::logging::LoggerMethod::Debug=>"debug",crate::logging::LoggerMethod::Info=>"info",crate::logging::LoggerMethod::Warn=>"warning",crate::logging::LoggerMethod::Error=>"error"};
                                    let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log(level,&text,None,None);
                                }
                            }
                            None=>(),
                        }
                    }
                }
            }
        });
    }
    async fn invalidate(&self,disabled:bool,clear_error:bool)->Result<(),McpError> {
        let transport={let mut inner=self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);inner.generation+=1;inner.pending=None;if clear_error{inner.last_error=None;}self.transition(&mut inner,if disabled{ServerConnectionState::Disabled}else{ServerConnectionState::Idle},None);inner.transport.take()};
        if let Some(transport)=transport{shutdown_mcp_transport(&transport).await?;}Ok(())
    }
    pub async fn bump_generation(&self)->Result<(),McpError> {self.invalidate(self.state()==ServerConnectionState::Disabled,true).await}
    pub async fn disable(&self)->Result<(),McpError> {self.invalidate(true,true).await}
    pub async fn dispose(&self)->Result<(),McpError> {self.invalidate(true,false).await}
    pub async fn renew(self:&Arc<Self>)->Result<Arc<McpClient>,McpError> {
        if self.state()==ServerConnectionState::Disabled{return Err(self.error(format!("MCP server {} is disabled",self.server_name),"renew"));}
        self.invalidate(false,false).await?;self.connect().await
    }
}
