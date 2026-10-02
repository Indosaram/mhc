use std::collections::HashMap;
use serde_json::{Value,json};
use tokio::sync::oneshot;

#[derive(Clone,Debug,PartialEq,Eq,thiserror::Error)]
#[error("Extension UI request cancelled: session closed")]
pub struct SessionClosed;
pub type UiRequestResult = Result<Value,SessionClosed>;
#[derive(Default)]
pub struct SessionExtensionUiRequests { pending:HashMap<String,oneshot::Sender<UiRequestResult>> }
impl SessionExtensionUiRequests {
    pub fn set(&mut self,id:String,request:oneshot::Sender<UiRequestResult>) {
        self.pending.insert(id,request);
    }
    pub fn delete(&mut self,id:&str) { self.pending.remove(id); }
    pub fn resolve(&mut self,response:Value) -> bool {
        let Some(id) = response.get("id").and_then(Value::as_str) else { return false; };
        let Some(request) = self.pending.remove(id) else { return false; };
        match request.send(Ok(response)) { Ok(()) | Err(_) => true }
    }
    pub fn cancel_all(&mut self) {
        for (id,request) in self.pending.drain() {
            match request.send(Ok(json!({"type":"extension_ui_response","id":id,"cancelled":true}))) { Ok(()) | Err(_) => {} }
        }
    }
    pub fn close(&mut self) {
        for (_,request) in self.pending.drain() {
            match request.send(Err(SessionClosed)) { Ok(()) | Err(_) => {} }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn response_resolves_only_matching_session_request() {
        let mut first = SessionExtensionUiRequests::default();
        let mut second = SessionExtensionUiRequests::default();
        let (sender,receiver) = oneshot::channel();
        first.set("id".into(),sender);
        let response = json!({"type":"extension_ui_response","id":"id","value":"ok"});
        assert!(!second.resolve(response.clone()));
        assert!(first.resolve(response.clone()));
        assert_eq!(receiver.await.unwrap().unwrap(),response);
    }
    #[tokio::test] async fn cancellation_resolves_all_requests_with_cancelled_records() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender,receiver) = oneshot::channel();
        requests.set("id".into(),sender);
        requests.cancel_all();
        assert_eq!(receiver.await.unwrap().unwrap(),json!({"type":"extension_ui_response","id":"id","cancelled":true}));
        assert!(!requests.resolve(json!({"id":"id"})));
    }
    #[tokio::test] async fn session_close_rejects_pending_request() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender,receiver) = oneshot::channel();
        requests.set("id".into(),sender);
        requests.close();
        assert_eq!(receiver.await.unwrap(),Err(SessionClosed));
    }
    #[test] fn deleted_request_is_not_resolved() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender,_receiver) = oneshot::channel();
        requests.set("id".into(),sender);
        requests.delete("id");
        assert!(!requests.resolve(json!({"id":"id"})));
    }
}
