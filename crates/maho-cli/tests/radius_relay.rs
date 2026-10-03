use maho_cli::experimental::radius_relay::*;
#[test]
fn relay_frame_roundtrip_and_invalid_headers() {
    let id = "11111111-2222-4333-8444-555555555555";
    let frame = encode_relay_data_frame(id, b"payload").unwrap();
    assert_eq!(parse_relay_data_frame(&frame), Some((id.to_owned(), b"payload".to_vec())));
    assert!(parse_relay_data_frame(&[1; 17]).is_none());
    let mut wrong = frame; wrong[0] = 2;
    assert!(parse_relay_data_frame(&wrong).is_none());
    assert!(encode_relay_data_frame("11111111-2222-1333-8444-555555555555", b"").is_err());
}
#[test]
fn relay_urls_replace_base_path_and_require_http() {
    assert_eq!(relay_websocket_url("https://example.com/base", "server").unwrap(), "wss://example.com/v1/session-relays/server/connect");
    assert!(relay_websocket_url("ftp://example.com", "server").is_err());
}
