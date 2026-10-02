use maho_ext_mcp::{commands::*,config_schema::Transport};
#[test]
fn quoted_endpoints_preserve_arguments_and_receive_production_defaults() {
    let args=split_command_args(r#"fixture node "argument with spaces" 'single quoted' "escaped\"quote""#).unwrap();
    assert_eq!(args,vec!["fixture","node","argument with spaces","single quoted","escaped\"quote"]);
    let config=parse_server_config(&args[1..]);assert_eq!(config.transport,Some(Transport::Stdio));assert_eq!(config.connect_timeout_ms,Some(15000.0));assert_eq!(config.args.unwrap().len(),3);
    let http=parse_server_config(&["https://example.test/mcp".into()]);assert_eq!(http.transport,Some(Transport::Http));assert_eq!(http.url.as_deref(),Some("https://example.test/mcp"));
}
