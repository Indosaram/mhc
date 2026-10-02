use tokio::io::{AsyncWrite,AsyncWriteExt};
pub const SOCKET_CUT_GRACE_MS:u64=5_000;
pub struct SocketSink<W>{writer:W,cut:bool}
impl<W:AsyncWrite+Unpin> SocketSink<W>{
    pub fn new(writer:W)->Self{Self{writer,cut:false}}
    pub async fn write_raw(&mut self,chunk:&str)->std::io::Result<()>{if self.cut{return Ok(());}self.writer.write_all(chunk.as_bytes()).await}
    pub async fn close(&mut self)->std::io::Result<()>{if self.cut{return Ok(());}self.cut=true;self.writer.shutdown().await}
    pub fn is_cut(&self)->bool{self.cut}
}
#[cfg(test)]mod tests{use super::*;use tokio::io::AsyncReadExt;#[tokio::test]async fn real_socket_half_close_delivers_notice_and_ignores_later_writes(){let(sender,mut reader)=tokio::net::UnixStream::pair().unwrap();let mut sink=SocketSink::new(sender);sink.write_raw(crate::socket_event_fanout::OVERFLOW_NOTICE).await.unwrap();sink.close().await.unwrap();sink.write_raw("ignored").await.unwrap();sink.close().await.unwrap();let mut received=String::new();tokio::time::timeout(std::time::Duration::from_secs(2),reader.read_to_string(&mut received)).await.unwrap().unwrap();assert_eq!(received,crate::socket_event_fanout::OVERFLOW_NOTICE);assert!(sink.is_cut());}}
