#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginListParams {
    #[serde(rename = "cwds", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwds: Option<Option<Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>>>,
    #[serde(rename = "marketplaceKinds", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub marketplace_kinds: Option<Option<Vec<Box<crate::app_server::protocol::generated::v2::plugin_list_marketplace_kind::PluginListMarketplaceKind>>>>,
    #[serde(rename = "forceRefetch", default, skip_serializing_if = "Option::is_none")]
    pub force_refetch: Option<bool>,
}
