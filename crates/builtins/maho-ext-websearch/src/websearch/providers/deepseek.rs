use super::{anthropic::build_anthropic_messages_search_request,shared::{BuildContext,BuiltSearchRequest}};
use crate::websearch::provider_endpoints::SearchProvider;
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>)->BuiltSearchRequest { build_anthropic_messages_search_request(ctx,SearchProvider::Deepseek,model,"deepseek-v4-flash") }
pub use super::anthropic::normalize_anthropic_messages_search_payload as normalize_response;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::websearch::{types::{SearchProviderConfig,SearchRequest,ConfigLoadResult,ConfigLoadFailureReason},config::load_websearch_config,providers::build_search_request};
    use serde_json::json;
    #[test] fn upstream_live_response_fixture_has_titled_http_results() {
        let payload=serde_json::from_str(include_str!("../../../tests/fixtures/websearch-deepseek-anthropic-response.json")).unwrap();
        let results=crate::websearch::providers::normalize_search_response(SearchProvider::Deepseek,&payload);
        assert!(!results.is_empty());for item in results {assert!(!item.title.is_empty());assert!(item.url.starts_with("https://") || item.url.starts_with("http://"));}
    }
    async fn load_config(key:Option<&str>)->ConfigLoadResult {
        let cwd=tempfile::tempdir().unwrap();let home=tempfile::tempdir().unwrap();
        tokio::fs::create_dir(cwd.path().join(".senpi")).await.unwrap();
        let mut entry=json!({"provider":"deepseek"});if let Some(key)=key {entry["apiKey"]=json!(key);}
        tokio::fs::write(cwd.path().join(".senpi/websearch.json"),json!({"auto":false,"providers":[entry]}).to_string()).await.unwrap();
        load_websearch_config(cwd.path(),home.path()).await.unwrap()
    }
    #[tokio::test] async fn upstream_config_accepts_deepseek_with_key() {
        let ConfigLoadResult::Ok{config,..}=load_config(Some("fixture-key")).await else {panic!("expected valid config")};
        assert_eq!(config.providers[0].config.provider,SearchProvider::Deepseek);
    }
    #[tokio::test] async fn upstream_config_requires_deepseek_key() {
        assert!(matches!(load_config(None).await,ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingApiKey,..}));
    }
    #[test] fn upstream_default_endpoint() {
        assert_eq!(crate::websearch::provider_endpoints::default_provider_url(SearchProvider::Deepseek),"https://api.deepseek.com/anthropic/v1/messages");
    }
    #[test] fn upstream_default_request_wire_contract() {
        let mut config=SearchProviderConfig::new(SearchProvider::Deepseek);config.api_key=Some("fixture-key".into());
        let request=SearchRequest{query:"rust release notes".into(),max_results:5.,allowed_domains:None,blocked_domains:None};
        let built=build_search_request(&config,&request).unwrap();assert_eq!(built.method,"POST");assert_eq!(built.headers["x-api-key"],"fixture-key");assert_eq!(built.headers["anthropic-version"],"2023-06-01");
        assert_eq!(built.url,"https://api.deepseek.com/anthropic/v1/messages");
        assert_eq!(built.body,json!({"model":"deepseek-v4-flash","max_tokens":1024,"messages":[{"role":"user","content":"rust release notes"}],"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":8}]}));
    }
    #[test] fn upstream_model_override_and_domain_filter() {
        let mut config=SearchProviderConfig::new(SearchProvider::Deepseek);config.api_key=Some("fixture-key".into());config.model=Some("deepseek-v4-pro".into());config.allowed_domains=Some(vec!["rust-lang.org".into()]);
        let built=build_search_request(&config,&SearchRequest{query:"rust release notes".into(),max_results:5.,allowed_domains:None,blocked_domains:None}).unwrap();
        assert_eq!(built.body["model"],"deepseek-v4-pro");assert_eq!(built.body["tools"],json!([{"type":"web_search_20250305","name":"web_search","max_uses":8,"allowed_domains":["rust-lang.org"]}]));
    }
    #[test] fn official_endpoint_and_default_model() { let req=build_request(&BuildContext{query:"q",max_results:1.,api_key:None,base_url:None,allowed_domains:None,blocked_domains:None},None); assert_eq!(req.url,"https://api.deepseek.com/anthropic/v1/messages"); assert_eq!(req.body["model"],"deepseek-v4-flash"); }
}
