use std::{collections::VecDeque,sync::Mutex};
use serde_json::Value;
use tokio::sync::Notify;
struct State {closed:bool,pending:VecDeque<Value>}
pub struct InputController {state:Mutex<State>,ready:Notify}
impl Default for InputController {fn default()->Self {Self {state:Mutex::new(State {closed:false,pending:VecDeque::new()}),ready:Notify::new()}}}
impl InputController {
    pub fn push(&self,message:Value)->anyhow::Result<()> {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.closed {anyhow::bail!("Cannot push to a closed session input controller");}state.pending.push_back(message);self.ready.notify_one();Ok(())
    }
    pub fn close(&self) {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.closed {return;}state.closed=true;self.ready.notify_waiters();}
    pub async fn next(&self)->Option<Value> {
        loop {
            let notified=self.ready.notified();tokio::pin!(notified);notified.as_mut().enable();
            {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(message)=state.pending.pop_front() {return Some(message);}
                if state.closed {return None;}}
            notified.await;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[tokio::test]
    async fn close_drains_pending_then_refuses_push() {let controller=InputController::default();controller.push(json!(1)).expect("push");controller.close();assert_eq!(controller.next().await,Some(json!(1)));assert_eq!(controller.next().await,None);assert!(controller.push(json!(2)).is_err());controller.close();}
    #[tokio::test]
    async fn close_wakes_all_registered_readers() {
        let controller=InputController::default();let mut first=std::pin::pin!(controller.next());let mut second=std::pin::pin!(controller.next());
        std::future::poll_fn(|cx| {assert!(first.as_mut().poll(cx).is_pending());assert!(second.as_mut().poll(cx).is_pending());std::task::Poll::Ready(())}).await;controller.close();assert!(first.await.is_none());assert!(second.await.is_none());
    }
}
