use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::Notify;

pub const SESSION_STREAM_QUEUE_CAPACITY: usize = 256;
enum Terminal<E> { Open, Closed, Failed(E) }
struct State<T,E> { values: VecDeque<T>, terminal: Terminal<E> }
/// A single-consumer queue. Buffered values drain before a terminal error.
pub struct BoundedAsyncQueue<T,E> {
    capacity: usize,
    state: Mutex<State<T,E>>,
    ready: Notify,
}
impl<T,E:Clone> BoundedAsyncQueue<T,E> {
    pub fn new(capacity:usize) -> Self {
        Self { capacity, state:Mutex::new(State {values:VecDeque::new(),terminal:Terminal::Open}), ready:Notify::new() }
    }
    pub fn push(&self,value:T) -> anyhow::Result<()> {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(state.terminal,Terminal::Open) { return Ok(()); }
        if state.values.len()>=self.capacity {
            anyhow::bail!("Anthropic Subscription session stream queue exceeded {} messages",self.capacity);
        }
        state.values.push_back(value);self.ready.notify_one();Ok(())
    }
    pub fn close(&self) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(state.terminal,Terminal::Open) { state.terminal=Terminal::Closed;self.ready.notify_one(); }
    }
    pub fn fail(&self,error:E) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(state.terminal,Terminal::Open) { state.terminal=Terminal::Failed(error);self.ready.notify_one(); }
    }
    pub async fn next(&self) -> Result<Option<T>,E> {
        loop {
            let notified=self.ready.notified();
            {
                let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(value)=state.values.pop_front() { return Ok(Some(value)); }
                match &state.terminal {
                    Terminal::Closed=>return Ok(None),
                    Terminal::Failed(error)=>return Err(error.clone()),
                    Terminal::Open=>{},
                }
            }
            notified.await;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn overflow_keeps_buffer_and_queue_open() {
        let queue=BoundedAsyncQueue::<_,String>::new(1);
        queue.push(1).expect("first value");assert!(queue.push(2).is_err());
        assert_eq!(queue.next().await,Ok(Some(1)));queue.push(3).expect("space reclaimed");
        queue.close();assert_eq!(queue.next().await,Ok(Some(3)));assert_eq!(queue.next().await,Ok(None));
    }
    #[tokio::test]
    async fn failure_drains_values_then_remains_failed() {
        let queue=BoundedAsyncQueue::new(2);queue.push(1).expect("value");queue.fail("first");queue.close();queue.fail("second");queue.push(2).expect("terminal push ignored");
        assert_eq!(queue.next().await,Ok(Some(1)));assert_eq!(queue.next().await,Err("first"));assert_eq!(queue.next().await,Err("first"));
    }
    #[tokio::test]
    async fn wakes_pending_consumer_without_timing() {
        let queue=BoundedAsyncQueue::<_,String>::new(1);
        let mut next=std::pin::pin!(queue.next());
        std::future::poll_fn(|cx| { assert!(next.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        queue.push(9).expect("wake");assert_eq!(next.await,Ok(Some(9)));
    }
    #[tokio::test]
    async fn closing_wakes_pending_consumer() {
        let queue=BoundedAsyncQueue::<u8,String>::new(1);let mut next=std::pin::pin!(queue.next());
        std::future::poll_fn(|cx| { assert!(next.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        queue.close();assert_eq!(next.await,Ok(None));
    }
}
