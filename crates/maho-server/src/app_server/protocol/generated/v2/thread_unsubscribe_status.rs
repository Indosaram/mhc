#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadUnsubscribeStatus {
    #[serde(rename = "notLoaded")]
    NotLoaded,
    #[serde(rename = "notSubscribed")]
    NotSubscribed,
    #[serde(rename = "unsubscribed")]
    Unsubscribed,
}
