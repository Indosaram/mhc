use super::command::ParsedCommandInput;
#[derive(Clone, PartialEq, Eq)]
pub enum AuthInput { Token(String), File(String) }
#[derive(Clone, PartialEq, Eq)]
pub enum TransportAddress { Unix(String), Radius(String) }
pub fn is_server_id(value: &str) -> bool { use std::sync::LazyLock; static ID: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new("^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").expect("server ID regex")); ID.is_match(value) }
pub fn parse_auth(input: &ParsedCommandInput) -> Result<Option<AuthInput>, String> {
    match (input.value("--auth-token"), input.value("--auth-token-file")) { (Some(_), Some(_)) => Err("--auth-token and --auth-token-file are mutually exclusive".to_owned()), (Some(token), None) => Ok(Some(AuthInput::Token(token.to_owned()))), (None, Some(path)) => Ok(Some(AuthInput::File(path.to_owned()))), _ => Ok(None) }
}
pub fn parse_transport_address(value: &str) -> Result<TransportAddress, String> {
    let invalid = || format!("Invalid --connect address \"{value}\""); let url = url::Url::parse(value).map_err(|_| invalid())?;
    if url.scheme() == "radius" {
        let host = url.host_str().unwrap_or(""); if !url.username().is_empty() || url.password().is_some() || url.port().is_some() || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() || value != format!("radius://{host}{}", url.path()) { return Err(invalid()); }
        if !is_server_id(host) { return Err("Radius transport address requires a lowercase UUIDv4 server ID".to_owned()); } return Ok(TransportAddress::Radius(host.to_owned()));
    }
    if url.scheme() != "unix" { return Err(format!("Unsupported --connect transport \"{}:\"", url.scheme())); }
    if url.host_str().is_some_and(|host| !host.is_empty()) || url.port().is_some() || !url.username().is_empty() || url.password().is_some() { return Err("Unix transport address must not include an authority".to_owned()); }
    if !value.starts_with("unix:///") || value.starts_with("unix:////") || value.contains(['?', '#']) || url.as_str() != value { return Err(invalid()); }
    let encoded = url.path(); for (index, _) in encoded.match_indices('%') { if !encoded.get(index + 1..index + 3).is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit())) { return Err(invalid()); } }
    let path = percent_encoding::percent_decode_str(encoded).decode_utf8().map_err(|_| invalid())?;
    if path.contains('\0') { return Err(invalid()); }
    if !path.starts_with('/') { return Err("Unix transport address requires an absolute path".to_owned()); }
    Ok(TransportAddress::Unix(path.into_owned()))
}
pub fn unsupported_options(command: &str, input: &ParsedCommandInput) -> Vec<String> { if input.remaining_args.is_empty() { Vec::new() } else { vec![format!("The experimental {command} command does not support existing CLI options yet")] } }
