use super::{connection::{InitializedConnection, parse_initialize_params}, envelope::{ClassifiedIncoming, populate_outbound_notification}, errors, notifications::SendMessage, registry::MethodRegistry};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

pub struct CoreConnection {
    pub initialized: Mutex<InitializedConnection>,
    pub send: SendMessage,
}
pub struct ServerCore {
    connections: BTreeMap<String, Arc<CoreConnection>>,
    pub registry: MethodRegistry,
    pub agent_home: String,
    pub version: String,
    pub os_type: String,
    pub os_release: String,
    pub arch: String,
    pub platform: String,
    pub on_disconnect: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub approvals: Option<Arc<std::sync::Mutex<super::approval_bridge::ApprovalBridge>>>,
    pub user_input: Option<Arc<std::sync::Mutex<super::user_input_bridge::UserInputBridge>>>,
}
impl ServerCore {
    pub fn new(agent_home: String, version: String, os_type: String, os_release: String, arch: String, platform: String) -> Self {
        Self { connections: BTreeMap::new(), registry: MethodRegistry::default(), agent_home, version, os_type, os_release, arch, platform, on_disconnect: None, approvals:None, user_input:None }
    }
    pub fn add_connection(&mut self, id: String, send: SendMessage) -> Arc<CoreConnection> {
        let connection = Arc::new(CoreConnection { initialized: Mutex::new(InitializedConnection::default()), send });
        self.connections.insert(id, connection.clone());
        connection
    }
    pub fn remove_connection(&mut self, id: &str) {
        if self.connections.remove(id).is_some() && let Some(on_disconnect) = &self.on_disconnect { on_disconnect(id.to_owned()); }
    }
    pub fn get_connection(&self, id: &str) -> Option<Arc<CoreConnection>> { self.connections.get(id).cloned() }
    pub async fn receive(&self, id: &str, envelope: ClassifiedIncoming) -> Result<(), errors::JsonRpcError> {
        let Some(connection) = self.connections.get(id) else { return Ok(()); };
        if connection.initialized.lock().await.state.is_some() && let Some(input) = &self.user_input {
            let routed = match &envelope {
                ClassifiedIncoming::Notification(message) if message["method"] == "item/tool/userInputProgress" => Some(input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).progress(&message["params"])),
                ClassifiedIncoming::Response(message) if !message["id"].is_null() => {
                    let result = input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve_response(message);
                    match result {
                        Ok(false) if self.approvals.as_ref().is_some_and(|bridge|bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve_response(message))=>Some(Ok(true)),
                        Ok(false)=>{(connection.send)(json!({"id":message["id"],"error":{"code":-32600,"message":"Unknown server request id"}})).await?;return Ok(());},
                        result=>Some(result),
                    }
                },
                _=>None,
            };
            if let Some(result) = routed {
                if let Err(error) = result {
                    let response_id = match &envelope {ClassifiedIncoming::Response(message)=>message["id"].clone(),_=>Value::Null};
                    (connection.send)(json!({"id":response_id,"error":{"code":-32602,"message":error.to_string()}})).await?;
                }
                return Ok(());
            }
        }
        if self.user_input.is_none() && let ClassifiedIncoming::Response(message) = &envelope
            && self.approvals.as_ref().is_some_and(|bridge|bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve_response(message)) {return Ok(());}
        let mut dispatch_connection = None;
        let response = match envelope {
            ClassifiedIncoming::Request(request) => {
                if request["method"] == "initialize" {
                    let mut state = connection.initialized.lock().await;
                    if state.state.is_some() { json!({"id":request["id"],"error":errors::already_initialized_error()}) }
                    else if let Some(params) = parse_initialize_params(&request["params"]) {
                        state.initialize(&params, &self.version, &self.os_type, &self.os_release, &self.arch);
                        json!({"id":request["id"],"result":state.initialize_response(&self.agent_home, &self.platform)?})
                    } else { json!({"id":request["id"],"error":errors::invalid_params_error()}) }
                } else {
                    let mut state = connection.initialized.lock().await.registry_connection();
                    state.id = id.to_owned();
                    dispatch_connection = Some(state.clone());
                    self.registry.dispatch(state, request).await
                }
            }
            ClassifiedIncoming::ProtocolInvalid(_) => json!({"id":null,"error":errors::invalid_request_error()}),
            ClassifiedIncoming::Notification(_) | ClassifiedIncoming::Response(_) => return Ok(()),
        };
        (connection.send)(response).await?;
        if let Some(connection) = dispatch_connection { connection.finish_response(); }
        Ok(())
    }
    pub async fn send_notification_to_connection(&self, id: &str, notification: Value, now: u64) -> Result<bool, errors::JsonRpcError> {
        let Some(connection) = self.connections.get(id) else { return Ok(false); };
        if !connection.initialized.lock().await.can_deliver_notification(notification["method"].as_str().unwrap_or_default()) { return Ok(false); }
        (connection.send)(populate_outbound_notification(notification, now)).await?;
        Ok(true)
    }
    pub async fn broadcast_notification(&self, notification: Value, now: u64) -> Result<usize, errors::JsonRpcError> {
        let mut delivered = 0;
        for id in self.connections.keys() {
            if self.send_notification_to_connection(id, notification.clone(), now).await? { delivered += 1; }
        }
        Ok(delivered)
    }
}
