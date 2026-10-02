use std::sync::LazyLock;
pub const RADIUS_RELAY_HOST_SUBPROTOCOL: &str = "pi-session-relay.host.v1";
pub const RADIUS_RELAY_CLIENT_SUBPROTOCOL: &str = "pi-session-relay.client.v1";
fn valid_connection_id(id: &str) -> bool {
    static PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").expect("connection ID regex"));
    PATTERN.is_match(id)
}
pub fn encode_relay_data_frame(connection_id: &str, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    if !valid_connection_id(connection_id) { return Err("Invalid Radius relay connection ID"); }
    let hex = connection_id.replace('-', "");
    let mut frame = vec![1, 1];
    for index in 0..16 { frame.push(u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("validated hexadecimal UUID")); }
    frame.extend_from_slice(payload);
    Ok(frame)
}
pub fn parse_relay_data_frame(frame: &[u8]) -> Option<(String, Vec<u8>)> {
    if frame.len() < 18 || frame[..2] != [1, 1] { return None; }
    let mut hex = String::new();
    use std::fmt::Write;
    for byte in &frame[2..18] { write!(hex, "{byte:02x}").expect("write to String"); }
    let id = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
    valid_connection_id(&id).then(|| (id, frame[18..].to_vec()))
}
pub fn relay_websocket_url(gateway: &str, server_id: &str) -> Result<String, String> {
    let mut url = url::Url::parse(gateway).and_then(|url| url.join(&format!("/v1/session-relays/{server_id}/connect"))).map_err(|error| error.to_string())?;
    let scheme = match url.scheme() { "https" => "wss", "http" => "ws", scheme => return Err(format!("Unsupported Radius gateway protocol: {scheme}:")) };
    url.set_scheme(scheme).map_err(|()| "Unable to set WebSocket scheme".to_owned())?;
    Ok(url.to_string())
}
