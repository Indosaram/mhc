#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeOutputAudioDeltaNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "audio")]
    pub audio: Box<crate::app_server::protocol::generated::v2::thread_realtime_audio_chunk::ThreadRealtimeAudioChunk>,
}
