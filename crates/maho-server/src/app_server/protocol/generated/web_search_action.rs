#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionSearch1 {
    #[serde(rename = "query", default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(rename = "queries", default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionOpenPage2 {
    #[serde(rename = "url", default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchActionFindInPage3 {
    #[serde(rename = "url", default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "pattern", default, skip_serializing_if = "Option::is_none")]
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
    #[serde(rename = "open_page")]
    OpenPage(Box<WebSearchActionOpenPage2>),
    #[serde(rename = "find_in_page")]
    FindInPage(Box<WebSearchActionFindInPage3>),
    #[serde(rename = "other")]
    Other(Box<WebSearchActionOther4>),
}
