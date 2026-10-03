use maho_ext_pi_websearch::websearch::{native::*,types::SearchProvider};
use std::sync::atomic::{AtomicUsize,Ordering};
struct Registry{calls:AtomicUsize}
macro_rules! native_case{
    ($name:ident,$provider:literal,$id:literal,$expected:expr)=>{
        #[tokio::test]async fn $name(){let registry=Registry{calls:AtomicUsize::new(0)};let model=NativeModelInfo{provider:$provider.into(),id:$id.into(),base_url:"https://gateway.example.com/v1".into()};let entry=build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("mapping: {error}"));let expected:Option<(SearchProvider,&str)>=$expected;
            match (entry,expected){(None,None)=>{},(Some(entry),Some((provider,resource)))=>{assert_eq!(entry.config.provider,provider);assert_eq!(entry.config.base_url,Some(format!("https://gateway.example.com/v1/{resource}")));assert_eq!(entry.config.model.as_deref(),Some($id));assert_eq!(entry.config.api_key.as_deref(),Some("native-test"));assert_eq!(entry.priority,Some(-1.0));},_=>panic!("native mapping mismatch")}
        }
    }
}
native_case!(gpt_sol,"openai","gpt-5.6-sol",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_terra,"openai","gpt-5.6-terra",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_55,"openai","gpt-5.5",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_fast,"openai","gpt-5.5-fast",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_54,"openai","gpt-5.4",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_pro,"openai","gpt-5-pro",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_5,"openai","gpt-5",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_41,"openai","gpt-4.1-mini",Some((SearchProvider::Openai,"responses")));
native_case!(gpt_4o,"openai","gpt-4o-mini-2026-01-01",Some((SearchProvider::Openai,"responses")));
native_case!(codex_rejected,"openai","gpt-5.3-codex",None);
native_case!(codex_spark_rejected,"openai","gpt-5.3-codex-spark",None);
native_case!(gpt_turbo_rejected,"openai","gpt-4-turbo",None);
native_case!(o3_rejected,"openai","o3",None);
native_case!(claude_opus,"anthropic","claude-opus-5",Some((SearchProvider::Anthropic,"messages")));
native_case!(claude_sonnet,"anthropic","claude-sonnet-5",Some((SearchProvider::Anthropic,"messages")));
native_case!(claude_fable,"anthropic","claude-fable-5",Some((SearchProvider::Anthropic,"messages")));
native_case!(claude_haiku,"anthropic","claude-haiku-4-5",Some((SearchProvider::Anthropic,"messages")));
native_case!(claude_48,"anthropic","claude-opus-4-8",Some((SearchProvider::Anthropic,"messages")));
native_case!(claude_dated,"anthropic","claude-sonnet-4-5-20250929",Some((SearchProvider::Anthropic,"messages")));
native_case!(nonclaude_rejected,"anthropic","not-a-claude-model",None);
native_case!(grok,"xai","grok-4.3",Some((SearchProvider::Xai,"responses")));
native_case!(openrouter_claude,"openrouter","anthropic/claude-opus-5",Some((SearchProvider::Anthropic,"messages")));
impl NativeModelRegistry for Registry{
    fn get_api_key_and_headers<'a>(&'a self,_:&'a NativeModelInfo)->NativeAuthFuture<'a>{self.calls.fetch_add(1,Ordering::SeqCst);Box::pin(async{Ok(NativeAuthResult::Success{api_key:Some("native-test".into()),headers:None})})}
}
#[tokio::test]
async fn maps_models_and_preserves_public_id(){
    let registry=Registry{calls:AtomicUsize::new(0)};
    for (provider,id,expected,resource) in [("openai","gpt-5.6-sol",SearchProvider::Openai,"responses"),("anthropic","claude-opus-5",SearchProvider::Anthropic,"messages"),("openrouter","anthropic/claude-opus-5",SearchProvider::Anthropic,"messages"),("zai","glm-5",SearchProvider::Zai,"chat/completions"),("kimi-coding","any",SearchProvider::Kimi,"search")]{
        let model=NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://gateway.example.com/v1/#fragment".into()};
        let entry=build_native_entry(Some(&model),Some(&registry),"custom").await.unwrap_or_else(|error|panic!("native entry: {error}")).unwrap_or_else(||panic!("missing native entry"));
        assert_eq!(entry.config.provider,expected);assert_eq!(entry.config.id.as_deref(),Some("custom"));assert_eq!(entry.config.model.as_deref(),Some(id));assert_eq!(entry.config.base_url,Some(format!("https://gateway.example.com/v1/{resource}")));assert_eq!(entry.priority,Some(-1.0));
    }
}
#[tokio::test]
async fn supported_upstream_endpoint_variants(){
    let registry=Registry{calls:AtomicUsize::new(0)};
    for (provider,id,base,expected,suffix) in [("anthropic","claude-opus-4-7","https://api.anthropic.com",SearchProvider::Anthropic,"/v1/messages"),("anthropic","claude-opus-4-7","https://anthropic.gateway.example.com/proxy",SearchProvider::Anthropic,"/v1/messages"),("kimi-coding","k2p7","https://api.kimi.com/coding",SearchProvider::Kimi,"/v1/search"),("perplexity","sonar-pro","https://gateway.example.com/v1",SearchProvider::Perplexity,"/chat/completions"),("z-ai","glm-4.6","https://gateway.example.com/v1",SearchProvider::Zai,"/chat/completions"),("zai","glm-4.6","https://gateway.example.com/v1",SearchProvider::Zai,"/chat/completions")]{let model=NativeModelInfo{provider:provider.into(),id:id.into(),base_url:base.into()};let entry=build_native_entry(Some(&model),Some(&registry),"native").await.expect("entry").expect("supported model");assert_eq!(entry.config.provider,expected);assert_eq!(entry.config.base_url,Some(format!("{base}{suffix}")));}
    for (id,expected,suffix) in [("openai/gpt-5.5",SearchProvider::Openai,"responses"),("anthropic/claude-opus-4-7",SearchProvider::Anthropic,"messages"),("xai/grok-4-fast",SearchProvider::Xai,"responses"),("perplexity/sonar-pro",SearchProvider::Perplexity,"chat/completions"),("z-ai/glm-4.6",SearchProvider::Zai,"chat/completions")]{let model=NativeModelInfo{provider:"openrouter".into(),id:id.into(),base_url:"https://gateway.example.com/v1".into()};let entry=build_native_entry(Some(&model),Some(&registry),"native").await.expect("entry").expect("supported model");assert_eq!(entry.config.provider,expected);assert_eq!(entry.config.base_url,Some(format!("https://gateway.example.com/v1/{suffix}")));assert_eq!(entry.config.model.as_deref(),Some(id));}
}
#[tokio::test]
async fn unsafe_and_unsupported_routes_do_not_resolve_auth(){
    let registry=Registry{calls:AtomicUsize::new(0)};
    for base in ["http://127.0.0.1/v1","https://localhost/v1","https://localhost./v1","https://sub.localhost./v1","https://127.1../v1","https://0177.0.0.1../v1","https://2130706433../v1","https://0x7f000001../v1","https://10.1../v1","https://[::1]/v1","https://[fd00::1]/v1","https://[fe80::1]/v1","https://user:pass@gateway.example.com/v1","not-a-url"]{
        let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5.5".into(),base_url:base.into()};assert!(build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("native entry: {error}")).is_none());
    }
    for id in ["gpt-5.3-codex","gpt-4-turbo","o3"]{
        let model=NativeModelInfo{provider:"openai".into(),id:id.into(),base_url:"https://gateway.example.com/v1".into()};assert!(build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("native entry: {error}")).is_none());
    }
    assert_eq!(registry.calls.load(Ordering::SeqCst),0);
}
#[tokio::test]
async fn missing_model_returns_none(){let registry=Registry{calls:AtomicUsize::new(0)};assert!(build_native_entry(None,Some(&registry),"native").await.unwrap_or_else(|error|panic!("entry: {error}")).is_none());}
#[tokio::test]
async fn missing_registry_returns_none(){let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5".into(),base_url:"https://gateway.example.com/v1".into()};assert!(build_native_entry(Some(&model),None,"native").await.unwrap_or_else(|error|panic!("entry: {error}")).is_none());}
#[tokio::test]
async fn missing_key_returns_none(){struct NoKey;impl NativeModelRegistry for NoKey{fn get_api_key_and_headers<'a>(&'a self,_:&'a NativeModelInfo)->NativeAuthFuture<'a>{Box::pin(async{Ok(NativeAuthResult::Success{api_key:None,headers:None})})}}let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5".into(),base_url:"https://gateway.example.com/v1".into()};assert!(build_native_entry(Some(&model),Some(&NoKey),"native").await.unwrap_or_else(|error|panic!("entry: {error}")).is_none());}
#[tokio::test]
async fn auth_failure_returns_none(){struct Failure;impl NativeModelRegistry for Failure{fn get_api_key_and_headers<'a>(&'a self,_:&'a NativeModelInfo)->NativeAuthFuture<'a>{Box::pin(async{Ok(NativeAuthResult::Failure{error:"missing".into()})})}}let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5".into(),base_url:"https://gateway.example.com/v1".into()};assert!(build_native_entry(Some(&model),Some(&Failure),"native").await.unwrap_or_else(|error|panic!("entry: {error}")).is_none());}
native_case!(openrouter_without_slash,"openrouter","no-slash",None);
#[tokio::test]
async fn endpoint_suffix_is_not_duplicated(){let registry=Registry{calls:AtomicUsize::new(0)};for (provider,id,resource) in [("openai","gpt-5","responses"),("anthropic","claude-opus-4","messages")]{let base=format!("https://gateway.example.com/v1/{resource}");let model=NativeModelInfo{provider:provider.into(),id:id.into(),base_url:base.clone()};let entry=build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("entry: {error}")).unwrap_or_else(||panic!("missing entry"));assert_eq!(entry.config.base_url,Some(base));}}
#[tokio::test]
async fn discovery_deduplicates_before_auth_and_keeps_active_first(){
    struct Discovery{calls:AtomicUsize}
    impl NativeModelRegistry for Discovery{
        fn get_api_key_and_headers<'a>(&'a self,model:&'a NativeModelInfo)->NativeAuthFuture<'a>{self.calls.fetch_add(1,Ordering::SeqCst);Box::pin(async move{if model.id=="claude-first"{Ok(NativeAuthResult::Failure{error:"no auth".into()})}else{Ok(NativeAuthResult::Success{api_key:Some("fixture".into()),headers:None})}})}
        fn get_available(&self)->Option<Vec<NativeModelInfo>>{Some([("openai","gpt-5.5"),("anthropic","claude-first"),("anthropic","claude-second"),("xai","grok-4.3")].into_iter().map(|(provider,id)|NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://gateway.example.com/v1".into()}).collect())}
    }
    let registry=Discovery{calls:AtomicUsize::new(0)};let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5".into(),base_url:"https://gateway.example.com/v1".into()};
    let entries=build_native_entries(Some(&model),Some(&registry)).await.unwrap_or_else(|error|panic!("discovery: {error}"));
    assert_eq!(entries.len(),2);assert_eq!(entries[0].config.id.as_deref(),Some("native"));assert_eq!(entries[1].config.provider,SearchProvider::Xai);
    let id=entries[1].config.id.as_deref().unwrap_or_else(||panic!("missing discovered ID"));assert!(id.starts_with("native-xai-"));assert_eq!(id.len(),27);assert_eq!(registry.calls.load(Ordering::SeqCst),3);
}
