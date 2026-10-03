pub use super::{provider_endpoints::SearchProvider,providers::shared::{BuiltSearchRequest,SearchResultItem}};
use serde_json::Value;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SearchContextSize { Low,Medium,High }
impl SearchContextSize { pub const fn as_str(self)->&'static str { match self { Self::Low=>"low",Self::Medium=>"medium",Self::High=>"high" } } }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CodexSearchMode { Cached,Live }
impl CodexSearchMode { pub const fn as_str(self)->&'static str { match self { Self::Cached=>"cached",Self::Live=>"live" } } }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum RoutingStrategy { Priority,RoundRobin,FillFirst }
#[derive(Clone,Debug,PartialEq)]
pub struct SearchProviderConfig {
    pub id:Option<String>,pub provider:SearchProvider,pub api_key:Option<String>,pub base_url:Option<String>,pub search_engine_id:Option<String>,pub max_results:Option<f64>,pub model:Option<String>,pub codex_mode:Option<CodexSearchMode>,pub search_context_size:Option<SearchContextSize>,pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>>,pub user_location:Option<Value>,pub timeout_ms:Option<f64>,
}
impl SearchProviderConfig { pub fn new(provider:SearchProvider)->Self { Self{id:None,provider,api_key:None,base_url:None,search_engine_id:None,max_results:None,model:None,codex_mode:None,search_context_size:None,allowed_domains:None,blocked_domains:None,user_location:None,timeout_ms:None} } }
#[derive(Clone,Debug,PartialEq)]
pub struct SearchProviderEntry { pub config:SearchProviderConfig,pub priority:Option<f64>,pub weight:Option<f64> }
#[derive(Clone,Debug,PartialEq)]
pub struct WebsearchConfig { pub strategy:RoutingStrategy,pub fallback:bool,pub auto:bool,pub providers:Vec<SearchProviderEntry> }
#[derive(Clone,Debug,PartialEq)]
pub struct SearchRequest { pub query:String,pub max_results:f64,pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>> }
#[derive(Clone,Debug,PartialEq)]
pub struct SearchAttempt { pub provider:SearchProvider,pub entry_id:Option<String>,pub duration_ms:f64,pub results_count:usize,pub error:Option<String> }
#[derive(Clone,Debug,PartialEq)]
pub struct SearchDetails { pub provider:SearchProvider,pub entry_id:Option<String>,pub query:String,pub results:Vec<SearchResultItem>,pub duration_ms:f64,pub truncated:bool,pub strategy:Option<RoutingStrategy>,pub attempts:Option<Vec<SearchAttempt>>,pub answer:Option<String>,pub error:Option<String> }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ConfigLoadFailureReason { MissingConfig,InvalidConfig,MissingApiKey,ProviderNativeBypass }
#[derive(Clone,Debug,PartialEq)]
pub enum ConfigLoadResult { Ok{config:WebsearchConfig,source:String},Err{reason:ConfigLoadFailureReason,message:String,source:Option<String>} }
#[derive(Clone,Debug,PartialEq)]
pub enum ProviderValidationResult { Ok(Box<SearchProviderEntry>),Err{reason:ConfigLoadFailureReason,message:String} }
