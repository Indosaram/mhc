use futures::{Stream,StreamExt};
use maho_tools::definition::AbortSignal;
pub async fn discard_body<S,E>(mut body:S,max_bytes:usize,signal:Option<&AbortSignal>) where S:Stream<Item=Result<Vec<u8>,E>>+Unpin {
    let mut drained=0usize;
    loop {
        let result=if let Some(signal)=signal {
            if signal.is_aborted() { break; }
            tokio::select! { result=body.next()=>result, ()=signal.cancelled()=>None }
        } else { body.next().await };
        let Some(Ok(chunk))=result else { break; };
        if chunk.len()>max_bytes-drained { break; }
        drained+=chunk.len();
    }
}
#[cfg(test)]
mod tests {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use super::*;
    #[tokio::test] async fn drains_with_byte_bound() { let count=Arc::new(AtomicUsize::new(0)); let count2=Arc::clone(&count); let stream=futures::stream::iter([Ok::<_,()>(vec![0;4]),Ok(vec![0;4]),Ok(vec![0;4])]).inspect(move |_|{count2.fetch_add(1,Ordering::SeqCst);}); discard_body(stream,4,None).await; assert_eq!(count.load(Ordering::SeqCst),2); }
    #[tokio::test] async fn already_aborted_does_not_read() { let signal=AbortSignal::default(); signal.abort(); discard_body(futures::stream::pending::<Result<Vec<u8>,()>>(),4,Some(&signal)).await; }
    #[tokio::test] async fn stream_error_is_discarded() { discard_body(futures::stream::iter([Err::<Vec<u8>,_>(())]),4,None).await; }
    #[tokio::test] async fn cancellation_interrupts_pending_chunk() { let signal=AbortSignal::default(); let cancel=signal.clone(); let operation=discard_body(futures::stream::pending::<Result<Vec<u8>,()>>(),4,Some(&signal)); let cancel_future=async move { cancel.abort(); }; tokio::join!(operation,cancel_future); }
}
