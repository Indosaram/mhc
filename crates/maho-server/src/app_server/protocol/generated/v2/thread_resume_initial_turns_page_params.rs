#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadResumeInitialTurnsPageParams {
    #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub limit: Option<Option<f64>>,
    #[serde(rename = "sortDirection", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sort_direction: Option<Option<Box<crate::app_server::protocol::generated::v2::sort_direction::SortDirection>>>,
    #[serde(rename = "itemsView", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub items_view: Option<Option<Box<crate::app_server::protocol::generated::v2::turn_items_view::TurnItemsView>>>,
}
