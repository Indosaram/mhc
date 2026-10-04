#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeStartTransportWebsocket1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeStartTransportWebrtc2 {
    #[serde(rename = "sdp")]
    pub sdp: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ThreadRealtimeStartTransport {
    #[serde(rename = "websocket")]
    Websocket(Box<ThreadRealtimeStartTransportWebsocket1>),
    #[serde(rename = "webrtc")]
    Webrtc(Box<ThreadRealtimeStartTransportWebrtc2>),
}
