use std::collections::VecDeque;
pub const DEFAULT_STALL_MS:u64=30_000;
pub const DEFAULT_QUEUE_BYTES:usize=64*1024*1024;
pub const OVERFLOW_NOTICE:&str="{\"type\":\"overflow\",\"error\":\"overflow, resync required\"}\n";
pub const STALL_NOTICE:&str="{\"type\":\"overflow\",\"error\":\"stalled, resync required\"}\n";
#[derive(Debug,thiserror::Error,PartialEq,Eq)]
#[error("socket event queue overflow: {queued_bytes} queued + {incoming_bytes} incoming > {max_queue_bytes} (incoming: {incoming_preview})")]
pub struct SocketEventQueueOverflowError{pub queued_bytes:usize,pub incoming_bytes:usize,pub max_queue_bytes:usize,pub incoming_preview:String}
pub struct QueueEntry{pub line:String,pub key:Option<String>,pub demoted_line:Option<String>,pub on_written:Option<Box<dyn FnOnce()+Send>>}
pub struct SocketEventQueue{queue:VecDeque<QueueEntry>,queued_bytes:usize,max_queue_bytes:usize,closed:bool}
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
    #[test]fn snapshots_demote_without_losing_deltas(){let mut queue=SocketEventQueue::default();queue.enqueue(entry("snapshot+delta",Some("delta"))).unwrap();queue.enqueue(entry("new snapshot+delta",Some("new delta"))).unwrap();assert_eq!(queue.queued_bytes(),23);let old=queue.next_write().unwrap();assert_eq!(old.line,"delta");assert!(old.key.is_none());assert_eq!(queue.next_write().unwrap().line,"new snapshot+delta");assert_eq!(queue.queued_bytes(),0);}
    #[test]fn overflow_is_utf8_bytes_and_closed_admission_is_inert(){let mut queue=SocketEventQueue::new(3);let error=queue.enqueue(entry("한글",None)).unwrap_err();assert_eq!(error.incoming_bytes,6);assert_eq!(queue.queued_bytes(),0);queue.enqueue(entry("a",None)).unwrap();assert!(queue.next_write().is_none());}
    #[test]fn undemotable_snapshots_stay_fifo(){let mut queue=SocketEventQueue::default();queue.enqueue(entry("old",None)).unwrap();queue.enqueue(entry("new",None)).unwrap();assert_eq!(queue.next_write().unwrap().line,"old");assert_eq!(queue.next_write().unwrap().line,"new");}
}
