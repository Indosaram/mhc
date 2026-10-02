#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RealtimeVoicesList {
    #[serde(rename = "v1")]
    pub v1: Vec<Box<crate::app_server::protocol::generated::realtime_voice::RealtimeVoice>>,
    #[serde(rename = "v2")]
    pub v2: Vec<Box<crate::app_server::protocol::generated::realtime_voice::RealtimeVoice>>,
    #[serde(rename = "defaultV1")]
    pub default_v1: Box<crate::app_server::protocol::generated::realtime_voice::RealtimeVoice>,
    #[serde(rename = "defaultV2")]
    pub default_v2: Box<crate::app_server::protocol::generated::realtime_voice::RealtimeVoice>,
}
