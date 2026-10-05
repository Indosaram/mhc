#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeAudioChunk {
    #[serde(rename = "data")]
    pub data: String,
    #[serde(rename = "sampleRate")]
    pub sample_rate: f64,
    #[serde(rename = "numChannels")]
    pub num_channels: f64,
    #[serde(rename = "samplesPerChannel", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub samples_per_channel: Option<f64>,
    #[serde(rename = "itemId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub item_id: Option<String>,
}
