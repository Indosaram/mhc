use maho_server::{
    client::service_wire::ServiceStateDecoder, server::state_codec::ServiceStateEncoder,
};
use serde_json::json;

#[test]
fn interns_on_second_use_and_scopes_omission_to_batch() {
    let mut encoder = ServiceStateEncoder::default();
    let mut decoder = ServiceStateDecoder::default();
    let snapshot = json!({"serviceId":"test","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{}],["s",["key"],1]]}]}]});
    let encoded = encoder.snapshot(&snapshot).unwrap();
    assert_eq!(decoder.snapshot(&encoded).unwrap(), snapshot);
    let update = json!({"type":"state","member":"state","sequence":1,"ops":[["s",["key"],2],["s",["key"],3]]});
    let encoded = encoder.update(&update).unwrap();
    assert_eq!(
        encoded["ops"],
        json!([["#", 0, ["key"]], ["s", 0, 2], ["s", 3]])
    );
    assert_eq!(decoder.update(&encoded).unwrap(), update);
    let next = json!({"type":"state","member":"state","sequence":2,"ops":[["s",["key"],4]]});
    let encoded = encoder.update(&next).unwrap();
    assert_eq!(encoded["ops"], json!([["s", 0, 4]]));
    assert_eq!(decoder.update(&encoded).unwrap(), next);
}
#[test]
fn replacement_resets_dictionary() {
    let mut encoder = ServiceStateEncoder::default();
    let mut decoder = ServiceStateDecoder::default();
    let snapshot = json!({"serviceId":"test","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{}],["s",["key"],1]]}]}]});
    decoder
        .snapshot(&encoder.snapshot(&snapshot).unwrap())
        .unwrap();
    let update = json!({"type":"state","member":"state","sequence":1,"ops":[["s",["key"],2],["r",{}],["s",["key"],3]]});
    let encoded = encoder.update(&update).unwrap();
    assert_eq!(encoded["ops"][3], json!(["s", ["key"], 3]));
    assert_eq!(decoder.update(&encoded).unwrap(), update);
}
