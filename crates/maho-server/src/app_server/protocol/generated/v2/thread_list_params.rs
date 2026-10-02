#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ThreadListParamsCwd1 {
    Variant0(Box<String>),
    Variant1(Box<Vec<String>>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadListParams {
    #[serde(rename = "cursor", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cursor: Option<Option<String>>,
    #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub limit: Option<Option<f64>>,
    #[serde(rename = "sortKey", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sort_key: Option<Option<Box<crate::app_server::protocol::generated::v2::thread_sort_key::ThreadSortKey>>>,
    #[serde(rename = "sortDirection", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sort_direction: Option<Option<Box<crate::app_server::protocol::generated::v2::sort_direction::SortDirection>>>,
    #[serde(rename = "modelProviders", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub model_providers: Option<Option<Vec<String>>>,
    #[serde(rename = "sourceKinds", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub source_kinds: Option<Option<Vec<Box<crate::app_server::protocol::generated::v2::thread_source_kind::ThreadSourceKind>>>>,
    #[serde(rename = "archived", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub archived: Option<Option<bool>>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<ThreadListParamsCwd1>>,
    #[serde(rename = "useStateDbOnly", default, skip_serializing_if = "Option::is_none")]
    pub use_state_db_only: Option<bool>,
    #[serde(rename = "searchTerm", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub search_term: Option<Option<String>>,
}
