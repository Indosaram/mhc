use crate::config_schema::{McpServerConfig,Transport};
pub fn parse_server_config(endpoint:&[String])->McpServerConfig {
    let first=endpoint.first().cloned().unwrap_or_default();
    crate::config::normalize_server(if first.starts_with("http://") || first.starts_with("https://") {McpServerConfig {transport:Some(Transport::Http),url:Some(first),..Default::default()}}else{McpServerConfig {transport:Some(Transport::Stdio),command:Some(first),args:Some(endpoint.iter().skip(1).cloned().collect()),..Default::default()}})
}
pub fn split_command_args(raw:&str)->Result<Vec<String>,regex::Error> {
    let matcher=regex::Regex::new(r#""([^"\\]*(?:\\.[^"\\]*)*)"|'([^'\\]*(?:\\.[^'\\]*)*)'|[^\s]+"#)?;
    let escape=regex::Regex::new(r#"\\(["'])"#)?;
    Ok(matcher.find_iter(raw).map(|part|{let part=part.as_str();let part=part.strip_prefix(['"','\'']).unwrap_or(part);let part=part.strip_suffix(['"','\'']).unwrap_or(part);escape.replace_all(part,"$1").into_owned()}).collect())
}
