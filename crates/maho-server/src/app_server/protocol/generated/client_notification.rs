#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ClientNotificationMethod1 {
    #[serde(rename = "initialized")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientNotification {
    #[serde(rename = "method")]
    pub method: ClientNotificationMethod1,
}
