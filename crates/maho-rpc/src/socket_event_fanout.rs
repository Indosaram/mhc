use std::collections::VecDeque;
pub const DEFAULT_STALL_MS:u64=30_000;
pub const DEFAULT_QUEUE_BYTES:usize=64*1024*1024;
pub const OVERFLOW_NOTICE:&str="{\"type\":\"overflow\",\"error\":\"overflow, resync required\"}\n";
pub const STALL_NOTICE:&str="{\"type\":\"overflow\",\"error\":\"stalled, resync required\"}\n";
#[derive(Debug,thiserror::Error,PartialEq,Eq)]
#[error("socket event queue stalled: peer did not drain {pending_bytes} queued bytes within {stall_ms}ms")]
pub struct SocketEventQueueStallError{pub pending_bytes:usize,pub stall_ms:u64}
pub struct DrainDeadline{blocked_mark:f64,stall_ms:u64}
impl DrainDeadline{
    pub fn new(stall_ms:u64,blocked:&crate::loop_blocked_time::LoopBlockedTime)->Self{Self{blocked_mark:blocked.loop_blocked_mark(),stall_ms}}
    pub fn on_deadline(&mut self,blocked:&crate::loop_blocked_time::LoopBlockedTime,pending_bytes:usize)->Result<f64,SocketEventQueueStallError>{let blocked_ms=blocked.loop_blocked_ms_since(self.blocked_mark);if blocked_ms>0.{self.blocked_mark=blocked.loop_blocked_mark();Ok(blocked_ms.min(self.stall_ms as f64))}else{Err(SocketEventQueueStallError{pending_bytes,stall_ms:self.stall_ms})}}
}
#[derive(Debug,thiserror::Error,PartialEq,Eq)]
#[error("socket event queue overflow: {queued_bytes} queued + {incoming_bytes} incoming > {max_queue_bytes} (incoming: {incoming_preview})")]
pub struct SocketEventQueueOverflowError{pub queued_bytes:usize,pub incoming_bytes:usize,pub max_queue_bytes:usize,pub incoming_preview:String}
pub struct QueueEntry{pub line:String,pub key:Option<String>,pub demoted_line:Option<String>,pub on_written:Option<Box<dyn FnOnce()+Send>>}
pub struct SocketEventQueue{queue:VecDeque<QueueEntry>,queued_bytes:usize,max_queue_bytes:usize,closed:bool}
pub struct SocketEventSinkActor{
    queue:std::sync::Arc<std::sync::Mutex<SocketEventQueue>>,
    changed:std::sync::Arc<tokio::sync::Notify>,
    completion:tokio::sync::watch::Receiver<Result<bool,String>>,
    completion_tx:tokio::sync::watch::Sender<Result<bool,String>>,
}
impl SocketEventSinkActor{
    pub fn new<W>(mut writer:W,max_queue_bytes:usize,stall_ms:u64,blocked:std::sync::Arc<std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>>,on_failure:impl Fn(String)+Send+'static)->Self
    where W:tokio::io::AsyncWrite+Unpin+Send+'static{
        use tokio::io::AsyncWriteExt;
        let queue=std::sync::Arc::new(std::sync::Mutex::new(SocketEventQueue::new(max_queue_bytes)));
        let changed=std::sync::Arc::new(tokio::sync::Notify::new());
        let (completion_tx,completion)=tokio::sync::watch::channel(Ok(true));
        let pending=queue.clone();let wake=changed.clone();
        let state=completion_tx.clone();
        tokio::spawn(async move{
            loop{
                let notified=wake.notified();
                let (entry,closed)={let mut queue=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let entry=queue.next_write();let healthy=completion_tx.borrow().is_ok();if healthy{let _=completion_tx.send_replace(Ok(entry.is_none()));}(entry,queue.closed)};
                let Some(mut entry)=entry else{if closed{
                    let failure=completion_tx.borrow().clone().err();
                    if let Some(error)=failure{
                        on_failure(error);
                        let _=tokio::time::timeout(std::time::Duration::from_millis(crate::socket_sink::SOCKET_CUT_GRACE_MS),async{writer.write_all(OVERFLOW_NOTICE.as_bytes()).await?;writer.shutdown().await}).await;
                    }
                    return;
                }notified.await;continue;};
                if entry.line.is_empty(){if let Some(written)=entry.on_written.take(){written();}continue;}
                let mut deadline=DrainDeadline::new(stall_ms,&blocked.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                let mut offset=0;
                let timer=tokio::time::sleep(std::time::Duration::from_millis(stall_ms));tokio::pin!(timer);
                let mut notice=STALL_NOTICE;
                let outcome=loop{
                    tokio::select!{
                        result=writer.write(&entry.line.as_bytes()[offset..])=>match result{
                            Ok(0)=>break Err("RPC socket write returned zero".to_owned()),
                            Ok(count)=>{offset+=count;if offset==entry.line.len(){break Ok(());}},
                            Err(error)=>break Err(error.to_string()),
                        },
                        ()=wake.notified()=>{
                            if pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).closed{
                                notice=OVERFLOW_NOTICE;
                                let failure=completion_tx.borrow().clone().err();
                                if let Some(error)=failure{break Err(error);}
                                let _=completion_tx.send_replace(Ok(true));
                                return;
                            }
                        },
                        ()=&mut timer=>{
                            let bytes=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).queued_bytes()+entry.line.len();
                            match deadline.on_deadline(&blocked.lock().unwrap_or_else(std::sync::PoisonError::into_inner),bytes){Ok(ms)=>timer.as_mut().reset(tokio::time::Instant::now()+std::time::Duration::from_secs_f64(ms/1000.)),Err(error)=>break Err(error.to_string())}
                        }
                    }
                };
                if let Err(error)=outcome{
                    pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).close();
                    let _=completion_tx.send_replace(Err(error.clone()));on_failure(error);
                    let _=tokio::time::timeout(std::time::Duration::from_millis(crate::socket_sink::SOCKET_CUT_GRACE_MS),async{
                        writer.write_all(&entry.line.as_bytes()[offset..]).await?;
                        writer.write_all(notice.as_bytes()).await?;
                        writer.shutdown().await
                    }).await;
                    return;
                }
                if let Some(written)=entry.on_written.take(){written();}
            }
        });
        Self{queue,changed,completion,completion_tx:state}
    }
    pub fn enqueue(&self,entry:QueueEntry)->Result<(),SocketEventQueueOverflowError>{
        let mut queue=self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.closed{return Ok(());}
        if let Err(error)=queue.enqueue(entry){
            let _=self.completion_tx.send_replace(Err(error.to_string()));
            self.changed.notify_one();return Err(error);
        }
        let _=self.completion_tx.send_replace(Ok(false));
        self.changed.notify_one();Ok(())
    }
    pub async fn flush(&mut self)->Result<(),String>{
        loop{
            let empty=self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).queued_bytes()==0;
            let state=self.completion.borrow_and_update().clone()?;
            if empty&&state{return Ok(());}
            self.completion.changed().await.map_err(|_|"Socket drain actor closed".to_owned())?;
        }
    }
    pub fn failure(&self)->Option<String>{self.completion.borrow().clone().err()}
    pub fn close(&self){
        if self.failure().is_some(){return;}
        self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).close();self.changed.notify_one();
    }
}
impl Drop for SocketEventSinkActor{fn drop(&mut self){self.close();}}
impl Default for SocketEventQueue{fn default()->Self{Self::new(DEFAULT_QUEUE_BYTES)}}
impl SocketEventQueue{
    pub fn new(max_queue_bytes:usize)->Self{Self{queue:VecDeque::new(),queued_bytes:0,max_queue_bytes,closed:false}}
    pub fn queued_bytes(&self)->usize{self.queued_bytes}
    pub fn enqueue(&mut self,entry:QueueEntry)->Result<(),SocketEventQueueOverflowError>{
        if self.closed{return Ok(());}
        if let Some(key)=&entry.key&&let Some(existing)=self.queue.iter_mut().find(|existing|existing.key.as_ref()==Some(key)){
            if let Some(demoted)=existing.demoted_line.take(){self.queued_bytes-=existing.line.len();existing.line=demoted;self.queued_bytes+=existing.line.len();}
            existing.key=None;
        }
        let bytes=entry.line.len();
        if self.queued_bytes+bytes>self.max_queue_bytes{
            let error=SocketEventQueueOverflowError{queued_bytes:self.queued_bytes,incoming_bytes:bytes,max_queue_bytes:self.max_queue_bytes,incoming_preview:String::from_utf16_lossy(&entry.line.encode_utf16().take(120).collect::<Vec<_>>())};
            self.close();return Err(error);
        }
        self.queue.push_back(entry);self.queued_bytes+=bytes;Ok(())
    }
    pub fn next_write(&mut self)->Option<QueueEntry>{let entry=self.queue.pop_front()?;self.queued_bytes-=entry.line.len();Some(entry)}
    pub fn close(&mut self){self.closed=true;self.queue.clear();self.queued_bytes=0;}
}
#[cfg(test)]mod tests{
    use super::*;
    fn entry(line:&str,demoted:Option<&str>)->QueueEntry{QueueEntry{line:line.into(),key:Some("text".into()),demoted_line:demoted.map(str::to_owned),on_written:None}}
    #[test]fn deadline_rearms_for_host_blocked_time_before_cutting_peer(){let mut blocked=crate::loop_blocked_time::LoopBlockedTime::default();let mut deadline=DrainDeadline::new(30000,&blocked);blocked.record_loop_blocked_ms(60000.);assert_eq!(deadline.on_deadline(&blocked,10).unwrap(),30000.);blocked.record_loop_blocked_ms(1000.);assert_eq!(deadline.on_deadline(&blocked,20).unwrap(),1000.);assert_eq!(deadline.on_deadline(&blocked,30),Err(SocketEventQueueStallError{pending_bytes:30,stall_ms:30000}));}
    #[test]fn snapshots_demote_without_losing_deltas(){let mut queue=SocketEventQueue::default();queue.enqueue(entry("snapshot+delta",Some("delta"))).unwrap();queue.enqueue(entry("new snapshot+delta",Some("new delta"))).unwrap();assert_eq!(queue.queued_bytes(),23);let old=queue.next_write().unwrap();assert_eq!(old.line,"delta");assert!(old.key.is_none());assert_eq!(queue.next_write().unwrap().line,"new snapshot+delta");assert_eq!(queue.queued_bytes(),0);}
    #[test]fn overflow_is_utf8_bytes_and_closed_admission_is_inert(){let mut queue=SocketEventQueue::new(3);let error=queue.enqueue(entry("한글",None)).unwrap_err();assert_eq!(error.incoming_bytes,6);assert_eq!(queue.queued_bytes(),0);queue.enqueue(entry("a",None)).unwrap();assert!(queue.next_write().is_none());}
    #[test]fn undemotable_snapshots_stay_fifo(){let mut queue=SocketEventQueue::default();queue.enqueue(entry("old",None)).unwrap();queue.enqueue(entry("new",None)).unwrap();assert_eq!(queue.next_write().unwrap().line,"old");assert_eq!(queue.next_write().unwrap().line,"new");}
}
