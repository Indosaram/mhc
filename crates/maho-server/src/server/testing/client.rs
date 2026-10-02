use crate::{protocol::{codec::{MessageDecoder,encode_client_message},framing::DEFAULT_MAX_FRAME_LENGTH,messages::PROTOCOL_VERSION},server::{errors::ServerError,types::ServerFuture}};
use serde_json::{Value,json};
use std::{path::Path,sync::{Arc,Mutex}};
use tokio::{io::{AsyncReadExt,AsyncWriteExt},net::{UnixStream,unix::OwnedWriteHalf},sync::{Mutex as AsyncMutex,watch}};

pub trait WireChannel:Send+Sync {
    fn send<'a>(&'a self,chunk:&'a [u8])->ServerFuture<'a,()>;
    fn send_fragmented<'a>(&'a self,chunk:&'a [u8],split_at:usize)->ServerFuture<'a,()>;
    fn close(&self)->ServerFuture<'_,()>;
}
struct ClientState {messages:Vec<Value>,decoder:MessageDecoder,attachment:Option<Value>,closed:bool,error:Option<ServerError>,request_sequence:u64}
pub struct ProtocolTestClient {channel:Arc<dyn WireChannel>,state:Mutex<ClientState>,changed:watch::Sender<u64>}
impl ProtocolTestClient {
    pub fn new(channel:Arc<dyn WireChannel>)->Self {
        Self {channel,state:Mutex::new(ClientState {messages:Vec::new(),decoder:MessageDecoder::server(),attachment:None,closed:false,error:None,request_sequence:0}),changed:watch::channel(0).0}
    }
    pub fn messages(&self)->Vec<Value> {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).messages.clone()}
    pub fn closed(&self)->bool {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).closed}
    pub async fn hello(&self,version:Option<u64>)->Result<Value,ServerError> {
        self.send_message(&json!({"type":"hello","version":version.unwrap_or(PROTOCOL_VERSION)})).await?;
        self.next(|message|matches!(message["type"].as_str(),Some("hello"|"hello_error"))).await
    }
    pub async fn request_service(&self,target:Value,call:Value,id:Option<String>)->Result<Value,ServerError> {
        let id=id.unwrap_or_else(|| {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.request_sequence+=1;format!("request-{}",state.request_sequence)});
        self.send_message(&json!({"type":"request","id":id,"target":target,"call":call})).await?;
        self.next(|message|message["type"]=="response" && message["id"]==id).await
    }
    pub async fn attach(&self,server_id:&str,session_id:&str)->Result<Value,ServerError> {
        self.request_service(json!({"serverId":server_id}),json!({"serviceId":"pi.session-management","member":"attach","args":[session_id]}),None).await
    }
    pub async fn request_session_service(&self,server_id:&str,session_id:&str,call:Value,id:Option<String>)->Result<Value,ServerError> {
        let target={let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let attachment_id=state.attachment.as_ref().filter(|attachment|attachment["sessionId"]==session_id).and_then(|attachment|attachment["attachmentId"].as_str()).unwrap_or("missing-attachment");
            json!({"serverId":server_id,"sessionId":session_id,"attachmentId":attachment_id})};
        self.request_service(target,call,id).await
    }
    pub async fn send_message(&self,message:&Value)->Result<(),ServerError> {
        let bytes=encode_client_message(message,DEFAULT_MAX_FRAME_LENGTH).map_err(|error|ServerError::new("invalid_request",&error.to_string()))?;self.channel.send(&bytes).await
    }
    pub async fn send_bytes(&self,bytes:&[u8])->Result<(),ServerError> {self.channel.send(bytes).await}
    pub async fn send_fragmented_message(&self,message:&Value,split_at:usize)->Result<(),ServerError> {
        let bytes=encode_client_message(message,DEFAULT_MAX_FRAME_LENGTH).map_err(|error|ServerError::new("invalid_request",&error.to_string()))?;self.channel.send_fragmented(&bytes,split_at).await
    }
    pub async fn next(&self,predicate:impl Fn(&Value)->bool)->Result<Value,ServerError> {self.next_from(0,predicate).await}
    pub async fn next_from(&self,index:usize,predicate:impl Fn(&Value)->bool)->Result<Value,ServerError> {
        let mut changed=self.changed.subscribe();
        loop {
            {let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(message)=state.messages.iter().skip(index).find(|message|predicate(message)) {return Ok(message.clone());}
                if let Some(error)=&state.error {return Err(error.clone());}
                if state.closed {return Err(ServerError::new("internal_error","Wire client is closed"));}}
            changed.changed().await.map_err(|error|ServerError::new("internal_error",&error.to_string()))?;
        }
    }
    pub async fn wait_for_close(&self) {let mut changed=self.changed.subscribe();while !self.closed() {if changed.changed().await.is_err() {return;}}}
    pub async fn close(&self)->Result<(),ServerError> {self.channel.close().await}
    pub fn receive(&self,chunk:&[u8]) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.decoder.push(chunk) {Ok(messages)=>for message in messages {
            if message["type"]=="attachment" {state.attachment=(!message["attachment"].is_null()).then(||message["attachment"].clone());}state.messages.push(message);
        },Err(error)=>state.error=Some(ServerError::new("invalid_request",&error.to_string()))}
        drop(state);self.changed.send_modify(|sequence|*sequence+=1);
    }
    pub fn mark_closed(&self) {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.closed=true;drop(state);self.changed.send_modify(|sequence|*sequence+=1);}
    pub fn fail(&self,error:ServerError) {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error=Some(error);self.changed.send_modify(|sequence|*sequence+=1);}
}
struct UnixChannel {writer:AsyncMutex<OwnedWriteHalf>,close:watch::Sender<bool>,closed:super::host::Deferred<()>}
impl WireChannel for UnixChannel {
    fn send<'a>(&'a self,chunk:&'a [u8])->ServerFuture<'a,()> {Box::pin(async move {self.writer.lock().await.write_all(chunk).await?;Ok(())})}
    fn send_fragmented<'a>(&'a self,chunk:&'a [u8],split_at:usize)->ServerFuture<'a,()> {Box::pin(async move {
        let split_at=split_at.min(chunk.len());let mut writer=self.writer.lock().await;writer.write_all(&chunk[..split_at]).await?;writer.write_all(&chunk[split_at..]).await?;Ok(())
    })}
    fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {
        if !*self.close.borrow() {self.writer.lock().await.shutdown().await?;self.close.send_replace(true);}
        self.closed.wait().await;Ok(())
    })}
}
pub async fn connect_unix_test_client(path:&Path)->Result<Arc<ProtocolTestClient>,ServerError> {
    let stream=UnixStream::connect(path).await?;let (mut reader,writer)=stream.into_split();let (close,mut closing)=watch::channel(false);
    let closed=super::host::Deferred::default();let channel=Arc::new(UnixChannel {writer:AsyncMutex::new(writer),close,closed:closed.clone()});
    let client=Arc::new(ProtocolTestClient::new(channel));let weak=Arc::downgrade(&client);
    tokio::spawn(async move {
        let mut bytes=[0;4096];loop {tokio::select! {
            result=reader.read(&mut bytes)=>match result {Ok(0)=>break,Ok(count)=>if let Some(client)=weak.upgrade() {client.receive(&bytes[..count]);} else {break;},Err(error)=> {if let Some(client)=weak.upgrade() {client.fail(error.into());}break;}},
            _=closing.changed()=>break,
        }}
        drop(reader);if let Some(client)=weak.upgrade() {client.mark_closed();}closed.resolve(());
    });Ok(client)
}
