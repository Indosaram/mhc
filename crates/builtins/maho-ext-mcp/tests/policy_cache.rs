use maho_ext_mcp::{catalog_cache::*,config_schema::*,expose::policy::*};
use serde_json::json;
#[derive(Clone,Debug,PartialEq)]
struct Entry(String);
impl CatalogIdentity for Entry {fn server(&self)->&str {"fx"} fn tool(&self)->&str {&self.0}}
fn entries(count:usize)->Vec<Entry> {(1..=count).map(|n|Entry(format!("tool_{n}"))).collect()}
#[test]
fn exclude_filter_wins_over_include() {
    let config = McpServerConfig {include_tools:Some(vec!["tool_[1-3]".into()]),exclude_tools:Some(vec!["tool_3".into()]),..Default::default()};
    let result = compute_mcp_exposure_policy(&entries(5),&config,&default_settings());
    assert_eq!(result.active_entries,vec![Entry("tool_1".into()),Entry("tool_2".into())]);
}
#[test]
fn threshold_boundary_selects_search_at_eleven() {
    assert_eq!(compute_mcp_exposure_policy(&entries(10),&McpServerConfig::default(),&default_settings()).mode,Exposure::Direct);
    assert_eq!(compute_mcp_exposure_policy(&entries(11),&McpServerConfig::default(),&default_settings()).mode,Exposure::Search);
}
#[test]
fn direct_tools_in_search_are_sorted() {
    let config = McpServerConfig {exposure:Some(Exposure::Search),direct_tools:Some(DirectTools::Patterns(vec!["tool_12".into(),"tool_2".into(),"tool_1".into()])),..Default::default()};
    let result = compute_mcp_exposure_policy(&entries(12),&config,&default_settings());
    assert_eq!(result.active_entries,vec![Entry("tool_1".into()),Entry("tool_12".into()),Entry("tool_2".into())]);
}
#[test]
fn large_catalog_has_no_default_active_tools() {
    let result = compute_mcp_exposure_policy(&entries(30),&McpServerConfig::default(),&default_settings());
    assert_eq!(result.registered_entries.len(),30);assert!(result.active_entries.is_empty());
}
#[test]
fn empty_filter_match_is_not_an_error() {
    let config = McpServerConfig {include_tools:Some(vec!["missing_*".into()]),..Default::default()};
    let result = compute_mcp_exposure_policy(&entries(5),&config,&default_settings());
    assert_eq!(result.mode,Exposure::Direct);assert!(result.registered_entries.is_empty());assert_eq!(result.warnings.len(),1);
}
#[test]
fn auto_never_selects_proxy() {
    let result = compute_mcp_exposure_policy(&entries(100),&McpServerConfig::default(),&default_settings());
    assert_eq!(result.mode,Exposure::Search);
}
#[test]
fn cache_rejects_expiry_and_hash_mismatch() {
    let cache = normalize_cache_file(&json!({"version":1,"servers":{"fx":{"configHash":"hash","fetchedAt":1000,"tools":[{"name":"tool","inputSchema":{}}]}}}));
    assert!(get_valid_cached_server(&cache,"fx","hash",1001.0).is_some());
    assert!(get_valid_cached_server(&cache,"fx","wrong",1001.0).is_none());
    assert!(get_valid_cached_server(&cache,"fx","hash",604801001.0).is_none());
}
#[test]
fn corrupt_cache_is_empty() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("cache")).unwrap();std::fs::write(get_mcp_catalog_cache_path(root.path()),"{not json").unwrap();
    assert!(read_mcp_catalog_cache(root.path()).servers.is_empty());
}
#[test]
fn invalid_tools_invalidate_server_entry() {
    let cache = normalize_cache_file(&json!({"version":1,"servers":{"fx":{"configHash":"hash","fetchedAt":1000,"tools":[{"name":"tool","inputSchema":null}]}}}));
    assert!(cache.servers.is_empty());
}
#[test]
fn atomic_cache_write_round_trips_catalog() {
    let root = tempfile::tempdir().unwrap();
    let server = McpCachedServerCatalog {config_hash:"hash".into(),fetched_at:1000.0,tools:vec![json!({"name":"tool","inputSchema":{}})],resources:vec![],prompts:vec![],instructions:None};
    write_mcp_cached_server(root.path(),"fx",server.clone()).unwrap();
    assert_eq!(read_mcp_catalog_cache(root.path()).servers["fx"],server);
}
