use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="kebab-case")]
pub enum SearchProvider{Exa,Tavily,Brave,DuckduckgoHtml,Serper,GoogleCse,#[serde(rename="z-ai")]Zai,Openai,Codex,Anthropic,Perplexity,Xai,Kimi}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum SearchContextSize{Low,Medium,High}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum CodexSearchMode{Cached,Live}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="kebab-case")]
pub enum RoutingStrategy{Priority,RoundRobin,FillFirst}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SearchProviderConfig{
    pub provider:SearchProvider,
    #[serde(skip_serializing_if="Option::is_none")]pub id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub api_key:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub base_url:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub search_engine_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub max_results:Option<f64>,
    #[serde(skip_serializing_if="Option::is_none")]pub model:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub codex_mode:Option<CodexSearchMode>,
    #[serde(skip_serializing_if="Option::is_none")]pub search_context_size:Option<SearchContextSize>,
    #[serde(skip_serializing_if="Option::is_none")]pub allowed_domains:Option<Vec<String>>,
    #[serde(skip_serializing_if="Option::is_none")]pub blocked_domains:Option<Vec<String>>,
    #[serde(skip_serializing_if="Option::is_none")]pub user_location:Option<SearchUserLocation>,
    #[serde(skip_serializing_if="Option::is_none")]pub timeout_ms:Option<f64>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SearchProviderEntry{
    #[serde(flatten)]pub config:SearchProviderConfig,
    #[serde(skip_serializing_if="Option::is_none")]pub priority:Option<f64>,
    #[serde(skip_serializing_if="Option::is_none")]pub weight:Option<f64>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct WebsearchConfig{pub strategy:RoutingStrategy,pub fallback:bool,pub auto:bool,pub providers:Vec<SearchProviderEntry>}
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct SearchUserLocation{
    #[serde(skip_serializing_if="Option::is_none")]pub country:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub region:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub city:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub timezone:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SearchRequest{pub query:String,pub max_results:f64,
    #[serde(skip_serializing_if="Option::is_none")]pub allowed_domains:Option<Vec<String>>,
    #[serde(skip_serializing_if="Option::is_none")]pub blocked_domains:Option<Vec<String>>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct BuiltSearchRequest{pub url:String,pub init:SearchRequestInit,#[serde(skip_serializing_if="Option::is_none")]pub body:Option<serde_json::Map<String,serde_json::Value>>}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SearchRequestInit{pub method:HttpMethod,pub headers:std::collections::BTreeMap<String,String>}
#[derive(Clone,Copy,Debug,Serialize,Deserialize)]
pub enum HttpMethod{#[serde(rename="GET")]Get,#[serde(rename="POST")]Post}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SearchResultItem{pub title:String,pub url:String,
    #[serde(skip_serializing_if="Option::is_none")]pub snippet:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub score:Option<f64>,
    #[serde(skip_serializing_if="Option::is_none")]pub source:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub published_at:Option<String>,
}
