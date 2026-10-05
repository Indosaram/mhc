use maho_ext_websearch::websearch::{config::{config_from_object,validate_websearch_config},types::*};
use serde_json::json;

#[test]
fn malformed_optional_modes_and_routing_use_source_defaults() {
    let config=config_from_object(&json!({"providers":[{"provider":"duckduckgo-html","codexMode":"other","searchContextSize":3}],"strategy":false,"fallback":"false","auto":0})).unwrap();
    assert_eq!(config.strategy,RoutingStrategy::Priority);
    assert!(config.fallback&&config.auto);
    assert_eq!(config.providers[0].config.codex_mode,None);
    assert_eq!(config.providers[0].config.search_context_size,None);
    assert!(validate_websearch_config(&config).is_ok());
}
#[test]
fn finite_negative_fields_and_fractional_priority_are_retained() {
    let config=config_from_object(&json!({"provider":"duckduckgo-html","maxResults":-2,"timeoutMs":-1,"priority":-0.5,"weight":-3})).unwrap();
    let provider=&config.providers[0];
    assert_eq!(provider.config.max_results,Some(-2.));
    assert_eq!(provider.config.timeout_ms,Some(-1.));
    assert_eq!(provider.priority,Some(-0.5));
    assert_eq!(provider.weight,Some(-3.));
    assert_eq!(validate_websearch_config(&config).unwrap_err().0,ConfigLoadFailureReason::InvalidConfig);
}
#[test]
fn zero_fields_follow_source_truthiness_distinctions() {
    let config=config_from_object(&json!({"provider":"duckduckgo-html","maxResults":0,"timeoutMs":0,"priority":0,"weight":0})).unwrap();
    let provider=&config.providers[0];
    assert_eq!(provider.config.max_results,None);
    assert_eq!(provider.config.timeout_ms,Some(0.));
    assert_eq!(provider.priority,None);
    assert_eq!(provider.weight,None);
    assert_eq!(validate_websearch_config(&config).unwrap_err().0,ConfigLoadFailureReason::InvalidConfig);
}

#[tokio::test]
async fn overflow_json_optional_numbers_are_ignored_at_file_boundary() {
    let cwd=tempfile::tempdir().unwrap();let home=tempfile::tempdir().unwrap();
    std::fs::create_dir(cwd.path().join(".senpi")).unwrap();
    std::fs::write(cwd.path().join(".senpi/websearch.json"),r#"{"provider":"duckduckgo-html","maxResults":1e400,"timeoutMs":-1e400,"priority":1e400,"weight":1e400}"#).unwrap();
    let ConfigLoadResult::Ok{config,..}=maho_ext_websearch::websearch::config::load_websearch_config(cwd.path(),home.path()).await.unwrap() else {panic!("JSON.parse accepts overflow numbers; optionalNumber ignores nonfinite values")};
    let provider=&config.providers[0];
    assert_eq!(provider.config.max_results,None);assert_eq!(provider.config.timeout_ms,None);
    assert_eq!(provider.priority,None);assert_eq!(provider.weight,None);
}
