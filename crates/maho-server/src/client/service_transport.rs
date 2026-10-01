use super::{Client, api::ServiceSubscription, errors::ClientError};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::{
    sync::{oneshot, watch},
    task::JoinSet,
};

pub type ServiceListener = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<(), ClientError>> + Send>> + Send + Sync,
>;
pub struct ClientServiceTransport {
    client: Arc<Client>,
    get_target: Arc<dyn Fn() -> Option<Value> + Send + Sync>,
}
impl ClientServiceTransport {
    pub fn new(
        client: Arc<Client>,
        get_target: Arc<dyn Fn() -> Option<Value> + Send + Sync>,
    ) -> Self {
        Self { client, get_target }
    }
    fn target(&self) -> Result<Value, ClientError> {
        (self.get_target)()
            .ok_or_else(|| ClientError::Disconnected("Remote service target is unavailable".into()))
    }
    pub async fn invoke(
        &self,
        call: Value,
        cancel: Option<watch::Receiver<Option<ClientError>>>,
    ) -> Result<Option<Value>, ClientError> {
        self.client.request(self.target()?, call, cancel).await
    }
    pub async fn subscribe(
        &self,
        service_id: &str,
        mode: &str,
        listener: ServiceListener,
    ) -> Result<CallbackSubscription, ClientError> {
        let mut subscription = self
            .client
            .subscribe(self.target()?, service_id, mode)
            .await?;
        let snapshot = subscription.snapshot.clone();
        let (activate, activated) = oneshot::channel();
        let (stop, mut stopping) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            tokio::select! {_=async {let _stopped=stopping.wait_for(|v|*v).await;}=>{},activated=activated=>{
                if activated.is_ok() {
                    subscription.start();
                    loop {
                        tokio::select! {_=async {let _stopped=stopping.wait_for(|v|*v).await;}=>break,update=subscription.updates.recv()=>{
                            let Some(update)=update else {break;};
                            if let Err(error)=listener(update).await {eprintln!("{error}");}
                        }}
                    }
                }
            }}
            subscription
        });
        Ok(CallbackSubscription {
            snapshot,
            activate: Some(activate),
            stop,
            tasks,
        })
    }
}
pub struct CallbackSubscription {
    pub snapshot: Value,
    activate: Option<oneshot::Sender<()>>,
    stop: watch::Sender<bool>,
    tasks: JoinSet<ServiceSubscription>,
}
impl CallbackSubscription {
    pub fn activate(&mut self) {
        if let Some(activate) = self.activate.take() {
            let _closed = activate.send(());
        }
    }
    pub async fn close(&mut self) -> Result<(), ClientError> {
        self.stop.send_replace(true);
        while let Some(result) = self.tasks.join_next().await {
            let mut subscription = result.map_err(|e| ClientError::Disconnected(e.to_string()))?;
            subscription.dispose().await?;
        }
        Ok(())
    }
}
