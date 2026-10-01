use maho_server::protocol::{
    cbor::{CborOptions, CborValue, encode_cbor},
    codec::*,
    framing::{DEFAULT_MAX_FRAME_LENGTH as MAX, FrameDecoder, encode_frame},
    messages::{PROTOCOL_VERSION, is_server_id},
};
use serde_json::{Value, json};

const ID: &str = "00000000-0000-4000-8000-000000000001";
fn hello() -> Value {
    json!({"type":"hello","version":8})
}
fn server_hello() -> Value {
    json!({"type":"hello","version":8,"serverId":ID})
}
fn request() -> Value {
    json!({"type":"request","id":"request-1","target":{"serverId":ID},"call":{"serviceId":"pi.models","member":"list","args":[]}})
}

#[test]
fn negotiates_protocol_version_eight() {
    assert_eq!(PROTOCOL_VERSION, 8);
    assert!(is_supported_protocol_version(8));
    assert!(!is_supported_protocol_version(7));
}
#[test]
fn accepts_integer_client_versions_for_negotiation() {
    for version in [0, 8, 9] {
        assert!(parse_client_message(&json!({"type":"hello","version":version})).is_ok());
    }
}
#[test]
fn rejects_invalid_client_hello() {
    for value in [
        json!({"type":"hello","version":"8"}),
        json!({"type":"hello","version":8.5}),
        json!({"type":"hello","version":8,"extra":true}),
    ] {
        assert!(parse_client_message(&value).is_err());
    }
}
#[test]
fn rejects_noncanonical_server_ids() {
    for id in [
        "",
        "server-1",
        "00000000-0000-7000-8000-000000000001",
        "00000000-0000-4000-7000-000000000001",
        "00000000-0000-4000-8000-00000000000A",
    ] {
        let mut value = request();
        value["target"]["serverId"] = json!(id);
        assert!(parse_client_message(&value).is_err());
        assert!(!is_server_id(id));
    }
}
#[test]
fn keeps_request_and_event_payloads_opaque() {
    let mut value = request();
    value["call"] = json!({"arbitrary":[true,["opaque"]]});
    assert_eq!(parse_client_message(&value).unwrap(), &value);
    assert!(parse_server_message(&json!({"type":"service_update","subscriptionId":"s","update":{"applicationDefined":true}})).is_ok());
}
#[test]
fn rejects_byte_string_opaque_payloads() {
    let value = CborValue::Map(vec![
        ("type".into(), CborValue::Text("response".into())),
        ("id".into(), CborValue::Text("r".into())),
        ("ok".into(), CborValue::Bool(true)),
        ("result".into(), CborValue::Bytes(vec![1])),
    ]);
    let wire = encode_frame(&encode_cbor(&value, CborOptions::default()).unwrap()).unwrap();
    assert!(MessageDecoder::server().push(&wire).is_err());
}
#[test]
fn validates_cancellation_envelope() {
    let mut value = json!({"type":"cancel","id":"r","target":{"serverId":ID}});
    assert!(parse_client_message(&value).is_ok());
    value["id"] = json!("");
    assert!(parse_client_message(&value).is_err());
    value["id"] = json!("r");
    value["extra"] = json!(true);
    assert!(parse_client_message(&value).is_err());
}
#[test]
fn validates_attachment_route_updates() {
    assert!(parse_server_message(&json!({"type":"attachment","attachment":{"serverId":ID,"sessionId":"s","attachmentId":"a"}})).is_ok());
    assert!(parse_server_message(&json!({"type":"attachment","attachment":null})).is_ok());
    assert!(
        parse_server_message(&json!({"type":"attachment","attachment":{"sessionId":"s"}})).is_err()
    );
}
#[test]
fn rejects_malformed_request_boundaries() {
    let mut value = request();
    value["id"] = json!("");
    assert!(parse_client_message(&value).is_err());
    value["id"] = json!("r");
    value["extra"] = json!(true);
    assert!(parse_client_message(&value).is_err());
}
#[test]
fn accepts_void_response_without_result() {
    assert!(parse_server_message(&json!({"type":"response","id":"r","ok":true})).is_ok());
}
#[test]
fn rejects_malformed_server_boundaries() {
    for value in [
        json!({"type":"hello","version":8,"serverId":"server-1"}),
        json!({"type":"response","id":"r","ok":true,"result":[],"extra":true}),
        json!({"type":"response","id":"r","ok":false,"error":{"code":"","message":"bad"}}),
    ] {
        assert!(parse_server_message(&value).is_err());
    }
}
#[test]
fn accepts_opaque_error_codes() {
    for code in [
        "wrong_server",
        "cancelled",
        "service_not_found",
        "application_error",
    ] {
        assert!(parse_server_message(&json!({"type":"response","id":"r","ok":false,"error":{"code":code,"message":"safe"}})).is_ok());
    }
}
#[test]
fn rejects_unknown_messages_and_fields() {
    let mut value = server_hello();
    value["snapshot"] = json!({});
    assert!(parse_server_message(&value).is_err());
    assert!(parse_server_message(&json!({"type":"unknown","event":{}})).is_err());
}
#[test]
fn does_not_parse_json_strings_as_messages() {
    assert!(parse_client_message(&json!(hello().to_string())).is_err());
    assert!(parse_server_message(&json!(server_hello().to_string())).is_err());
}
#[test]
fn encodes_complete_client_and_server_frames() {
    assert_eq!(
        MessageDecoder::client()
            .push(&encode_client_message(&hello(), MAX).unwrap())
            .unwrap(),
        vec![hello()]
    );
    assert_eq!(
        MessageDecoder::server()
            .push(&encode_server_message(&server_hello(), MAX).unwrap())
            .unwrap(),
        vec![server_hello()]
    );
    assert_eq!(
        FrameDecoder::default()
            .push(&encode_client_message(&hello(), MAX).unwrap())
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn enforces_outbound_frame_limits() {
    assert!(encode_client_message(&hello(), 8).is_err());
    assert!(encode_server_message(&server_hello(), 8).is_err());
}
#[test]
fn incrementally_decodes_client_messages_at_every_split() {
    let wire = [
        encode_client_message(&hello(), MAX).unwrap(),
        encode_client_message(&request(), MAX).unwrap(),
    ]
    .concat();
    for split in 0..=wire.len() {
        let mut decoder = MessageDecoder::client();
        let messages = [
            decoder.push(&wire[..split]).unwrap(),
            decoder.push(&wire[split..]).unwrap(),
        ]
        .concat();
        decoder.end().unwrap();
        assert_eq!(messages, vec![hello(), request()]);
    }
}
#[test]
fn incrementally_decodes_server_messages() {
    let response = json!({"type":"response","id":"r","ok":true,"result":[]});
    let first = encode_server_message(&server_hello(), MAX).unwrap();
    let second = encode_server_message(&response, MAX).unwrap();
    let split = first.len() + second.len() / 2;
    let wire = [first, second].concat();
    let mut decoder = MessageDecoder::server();
    assert_eq!(decoder.push(&wire[..split]).unwrap(), vec![server_hello()]);
    assert_eq!(decoder.push(&wire[split..]).unwrap(), vec![response]);
    decoder.end().unwrap();
}
#[test]
fn rejects_invalid_frames_and_latches_failure() {
    for payload in [
        vec![],
        vec![0xff],
        encode_cbor(
            &CborValue::from_json(&json!({"type":"hello","version":1,"extra":true})).unwrap(),
            CborOptions::default(),
        )
        .unwrap(),
    ] {
        let mut decoder = MessageDecoder::client();
        assert!(decoder.push(&encode_frame(&payload).unwrap()).is_err());
        assert!(
            decoder
                .push(&encode_client_message(&hello(), MAX).unwrap())
                .unwrap_err()
                .to_string()
                .contains("failed")
        );
    }
}
#[test]
fn rejects_truncated_and_oversized_framing() {
    let mut decoder = MessageDecoder::server();
    assert!(decoder.push(&[0, 0, 0, 2, 1]).unwrap().is_empty());
    assert!(decoder.end().is_err());
    assert!(
        MessageDecoder::new(MessageKind::Client, 3)
            .push(&[0, 0, 0, 4])
            .is_err()
    );
}
