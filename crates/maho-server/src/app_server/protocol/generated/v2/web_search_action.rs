#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionSearch1 {
    #[serde(rename = "query", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub query: Option<String>,
    #[serde(rename = "queries", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub queries: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionOpenPage2 {
    #[serde(rename = "url", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionFindInPage3 {
    #[serde(rename = "url", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub url: Option<String>,
    #[serde(rename = "pattern", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub pattern: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionOther4 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum WebSearchAction {
    #[serde(rename = "search")]
    Search(Box<WebSearchActionSearch1>),
    #[serde(rename = "openPage")]
    OpenPage(Box<WebSearchActionOpenPage2>),
    #[serde(rename = "findInPage")]
    FindInPage(Box<WebSearchActionFindInPage3>),
    #[serde(rename = "other")]
    Other(Box<WebSearchActionOther4>),
}
