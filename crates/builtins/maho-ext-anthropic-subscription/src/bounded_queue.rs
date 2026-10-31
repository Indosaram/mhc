use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::Notify;

pub const SESSION_STREAM_QUEUE_CAPACITY: usize = 256;
enum Terminal<E> { Open, Closed, Failed(E) }
struct State<T,E> { values: VecDeque<T>, terminal: Terminal<E> }
/// A single-consumer queue with backpressure: once `capacity` values wait, the producer awaits [`Self::writable`]
/// before it reads more from its source, so a long streamed tool call never overflows it (senpi#2822). Buffered
/// values drain before a terminal error.
pub struct BoundedAsyncQueue<T,E> {
    capacity: usize,
    state: Mutex<State<T,E>>,
    ready: Notify,
    room: Notify,
}
impl<T,E:Clone> BoundedAsyncQueue<T,E> {
    pub fn new(capacity:usize) -> Self {
        Self { capacity, state:Mutex::new(State {values:VecDeque::new(),terminal:Terminal::Open}), ready:Notify::new(), room:Notify::new() }
    }
    /// Never refuses a value: the producer bounds the queue by awaiting [`Self::writable`] (senpi#2822).
    pub fn push(&self,value:T) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(state.terminal,Terminal::Open) { return; }
        state.values.push_back(value);self.ready.notify_one();
    }
    /// Resolves once there is room for more values, or once the queue has ended.
    pub async fn writable(&self) {
        loop {
            let room=self.room.notified();
            {
                let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !matches!(state.terminal,Terminal::Open) || state.values.len()<self.capacity { return; }
            }
            room.await;
        }
    }
    pub fn close(&self) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(state.terminal,Terminal::Open) { state.terminal=Terminal::Closed;self.ready.notify_one();self.room.notify_waiters(); }
    }
    pub fn fail(&self,error:E) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(state.terminal,Terminal::Open) { state.terminal=Terminal::Failed(error);self.ready.notify_one();self.room.notify_waiters(); }
    }
    pub async fn next(&self) -> Result<Option<T>,E> {
        loop {
            let notified=self.ready.notified();
            {
                let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(value)=state.values.pop_front() {
                    if state.values.len()<self.capacity { self.room.notify_waiters(); }
                    return Ok(Some(value));
                }
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
    async fn a_value_past_capacity_is_kept_not_refused() {
        let queue=BoundedAsyncQueue::<_,String>::new(1);
        queue.push(1);queue.push(2);
        assert_eq!(queue.next().await,Ok(Some(1)));queue.push(3);
        queue.close();assert_eq!(queue.next().await,Ok(Some(2)));assert_eq!(queue.next().await,Ok(Some(3)));assert_eq!(queue.next().await,Ok(None));
    }
    #[tokio::test]
    async fn failure_drains_values_then_remains_failed() {
        let queue=BoundedAsyncQueue::new(2);queue.push(1);queue.fail("first");queue.close();queue.fail("second");queue.push(2);
        assert_eq!(queue.next().await,Ok(Some(1)));assert_eq!(queue.next().await,Err("first"));assert_eq!(queue.next().await,Err("first"));
    }
    #[tokio::test]
    async fn wakes_pending_consumer_without_timing() {
        let queue=BoundedAsyncQueue::<_,String>::new(1);
        let mut next=std::pin::pin!(queue.next());
        std::future::poll_fn(|cx| { assert!(next.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        queue.push(9);assert_eq!(next.await,Ok(Some(9)));
    }
    #[tokio::test]
    async fn a_full_queue_holds_the_producer_until_the_consumer_drains() {
        let queue=BoundedAsyncQueue::<u8,String>::new(1);
        queue.writable().await;
        queue.push(1);
        let mut writable=std::pin::pin!(queue.writable());
        std::future::poll_fn(|cx| { assert!(writable.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        assert_eq!(queue.next().await,Ok(Some(1)));
        writable.await;
        queue.push(2);
        let mut held=std::pin::pin!(queue.writable());
        std::future::poll_fn(|cx| { assert!(held.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        queue.close();
        held.await;
    }
    #[tokio::test]
    async fn closing_wakes_pending_consumer() {
        let queue=BoundedAsyncQueue::<u8,String>::new(1);let mut next=std::pin::pin!(queue.next());
        std::future::poll_fn(|cx| { assert!(next.as_mut().poll(cx).is_pending());std::task::Poll::Ready(()) }).await;
        queue.close();assert_eq!(next.await,Ok(None));
    }
}
