use serde_json::{Map,Value,json};
use std::{sync::{Arc,atomic::{AtomicU64,Ordering}},time::Duration,io};
use tokio::{io::{AsyncReadExt,AsyncWriteExt},sync::oneshot};
use std::sync::Mutex;
static SEQUENCE:AtomicU64=AtomicU64::new(0);
pub fn herdr_socket_target(path:&str,platform:&str)->String{
    let lower=path.to_lowercase();
    if platform!="win32"||lower.starts_with("\\\\.\\pipe\\")||lower.starts_with("\\\\?\\pipe\\"){path.into()}else{format!("\\\\.\\pipe\\{}",path.replace('/',"\\"))}
}
#[derive(Clone,Copy)]
pub enum HerdrMethod{ReportAgent,ReportMetadata,ReportAgentSession,ReleaseAgent}
impl HerdrMethod{pub fn as_str(self)->&'static str{match self{Self::ReportAgent=>"pane.report_agent",Self::ReportMetadata=>"pane.report_metadata",Self::ReportAgentSession=>"pane.report_agent_session",Self::ReleaseAgent=>"pane.release_agent"}}}
pub struct HerdrClient{target:String,pane_id:String,now:Arc<dyn Fn()->u64+Send+Sync>,queue:Mutex<Option<oneshot::Receiver<()>>>}
impl HerdrClient{
    pub fn drain(&self)->impl std::future::Future<Output=()>+Send+use<> {
        let mut queue=self.queue.lock().expect("enqueue lock");let previous=queue.take();let (finished,next)=oneshot::channel();*queue=Some(next);
        async move{if let Some(previous)=previous{let _completion=previous.await;}let _completion=finished.send(());}
    }
    pub fn new(socket_path:String,pane_id:String,now:Arc<dyn Fn()->u64+Send+Sync>)->Self{
        let platform=if cfg!(windows){"win32"}else{"linux"};
        Self{target:herdr_socket_target(&socket_path,platform),pane_id,now,queue:Mutex::new(None)}
    }
    pub fn send(&self,method:HerdrMethod,mut params:Map<String,Value>)->impl std::future::Future<Output=io::Result<()>>+Send+use<> {
        let mut queue=self.queue.lock().expect("enqueue lock");
        let previous_request=queue.take();let (finished,next)=oneshot::channel();*queue=Some(next);drop(queue);
        let floor=(self.now)().saturating_mul(1000);let previous=SEQUENCE.fetch_update(Ordering::SeqCst,Ordering::SeqCst,|sequence|Some(sequence.saturating_add(1).max(floor))).expect("sequence update");let sequence=previous.saturating_add(1).max(floor);
        params.insert("pane_id".into(),self.pane_id.clone().into());params.insert("source".into(),"custom:senpi".into());params.insert("seq".into(),sequence.into());
        let request=json!({"id":format!("custom:senpi:{sequence}"),"method":method.as_str(),"params":params});
        let target=self.target.clone();
        async move{
            if let Some(previous)=previous_request{let _completion=previous.await;}
            let result=async{for timeout in [500,1500]{if tokio::time::timeout(Duration::from_millis(timeout),Self::attempt(&target,&request)).await.is_ok_and(|result|result.unwrap_or(false)){return Ok(());}}
                Err(io::Error::other(format!("Herdr request failed after two attempts: {}",method.as_str())))
            }.await;
            let _completion=finished.send(());result
        }
    }
    async fn attempt(target:&str,request:&Value)->io::Result<bool>{
        #[cfg(unix)]
        let mut socket=tokio::net::UnixStream::connect(target).await?;
        #[cfg(windows)]
        let mut socket=tokio::net::windows::named_pipe::ClientOptions::new().open(target)?;
        let mut bytes=serde_json::to_vec(request)?;bytes.push(b'\n');socket.write_all(&bytes).await?;
        let mut buffer=Vec::new();let mut chunk=[0u8;4096];
        loop{
            let count=socket.read(&mut chunk).await?;if count==0{return Ok(false);}buffer.extend_from_slice(&chunk[..count]);
            if buffer.len()>65_536{return Ok(false);}
            if let Some(end)=buffer.iter().position(|byte|*byte==b'\n'){
                let Ok(response)=serde_json::from_slice::<Value>(&buffer[..end])else{return Ok(false)};
                return Ok(response.is_object()&&response.get("id")==request.get("id")&&response.get("result").is_some()&&response.get("error").is_none());
            }
        }
    }
}
