#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "items")]
    pub items: Vec<Box<crate::app_server::protocol::generated::v2::thread_item::ThreadItem>>,
    #[serde(rename = "itemsView")]
    pub items_view: Box<crate::app_server::protocol::generated::v2::turn_items_view::TurnItemsView>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::turn_status::TurnStatus>,
    #[serde(rename = "error", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error: Option<Box<crate::app_server::protocol::generated::v2::turn_error::TurnError>>,
    #[serde(rename = "startedAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub started_at: Option<f64>,
    #[serde(rename = "completedAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub completed_at: Option<f64>,
    #[serde(rename = "durationMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub duration_ms: Option<f64>,
}
