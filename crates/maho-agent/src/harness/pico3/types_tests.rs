use super::*;

#[test]
fn memo_first_writer_includes_null_and_reserved_data_keys() {
    let mut slot=JsonObject::new();
    assert_eq!(memo_once(&mut slot,"__proto__",JsonValue::Null).unwrap(),JsonValue::Null);
    assert_eq!(memo_once(&mut slot,"__proto__",JsonValue::Bool(true)).unwrap(),JsonValue::Null);
    assert_eq!(memo_once(&mut slot,"other",JsonValue::Bool(false)).unwrap(),JsonValue::Bool(false));
    slot.insert("memos".into(),JsonValue::Null);
    assert!(matches!(memo_once(&mut slot,"x",JsonValue::Null),Err(Error::InvalidShape(_))));
}
#[test]
fn object_and_stored_preserve_surrogate_keys_and_values() {
    let key=JsString::from_utf16(vec![0xd800]);
    let value=JsonValue::String(JsString::from_utf16(vec![0xdc00]));
    let mut object=JsonObject::new(); object.insert(key.clone(),value.clone());
    let encoded=serde_json::to_string(&object).unwrap();
    let decoded:JsonObject=serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.get_key(&key),Some(&value));
    let stored=Stored::<Message>::from_json(JsonValue::object(decoded.into_inner()));
    let copied:Stored<Message>=serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
    assert_eq!(copied.as_json(),stored.as_json());
}
#[test]
fn patch_distinguishes_absent_null_and_replacement() {
    let absent:TaskPatch=serde_json::from_str(r#"{"id":1}"#).unwrap();
    let clear:TaskPatch=serde_json::from_str(r#"{"id":1,"checkpoint":null}"#).unwrap();
    let set:TaskPatch=serde_json::from_str(r#"{"id":1,"checkpoint":{"phase":"ready","n":2}}"#).unwrap();
    assert!(absent.checkpoint.is_none());
    assert!(matches!(clear.checkpoint,Some(None)));
    assert_eq!(set.checkpoint.unwrap().unwrap().phase,"ready");
    assert_eq!(serde_json::to_string(&clear).unwrap(),r#"{"id":1,"checkpoint":null}"#);
}
#[test]
fn witnesses_reject_reserved_names_and_tokens_use_identity() {
    assert!(define_task("pi.tool").is_err());
    assert!(define_entry::<Entry>("pi.user").is_err());
    let a=define_task("custom").unwrap(); let b=define_task("custom").unwrap();
    assert!(a.same_identity(&a.clone())); assert!(!a.same_identity(&b));
    let token=InvocationToken::new(3,InvocationMode::Run);
    assert!(token.check(3,InvocationMode::Run).is_ok());
    assert!(token.check(4,InvocationMode::Run).is_err());
    assert!(token.check(3,InvocationMode::Abort).is_err());
    let retained=token.clone(); token.revoke(); assert!(!retained.alive());
    assert!(retained.check(3,InvocationMode::Run).is_err());
}
#[test]
fn namespace_revocation_and_event_shapes_are_typed() {
    let ns=Namespace::<JsonObject>::new("acme");
    let other=Namespace::<JsonObject>::new("acme");
    assert!(!ns.same_identity(&other)); assert!(ns.same_identity(&ns.clone()));
    let event=ViewEvent::plugin(&ns,"done",JsonValue::Null).unwrap();
    assert_eq!(serde_json::to_string(&event).unwrap(),r#"{"type":"plugin.acme.done","data":null}"#);
    ns.unregister(); assert!(!ns.clone().is_registered());
    assert!(ViewEvent::plugin(&ns,"done",JsonValue::Null).is_err());
    assert!(serde_json::from_str::<True>("false").is_err());
}
#[test]
fn durable_documents_have_typed_fields_and_extension_objects() {
    let raw=r#"{"thinkingLevel":"off","selectedTools":[],"profile":"default","threshold":0.8,"keepRecent":2,"plugins":{},"extra":null}"#;
    let doc:RewindableState=serde_json::from_str(raw).unwrap();
    assert_eq!(doc.fields.get("extra"),Some(&JsonValue::Null));
    let round:RewindableState=serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
    assert_eq!(round.keep_recent,2);
    let write=Write::Doc {r#ref:DocRef::Sticky {conversation_id:8},ops:vec![Op::Replace(JsonValue::Null)]};
    let json=serde_json::to_string(&write).unwrap();
    assert!(matches!(serde_json::from_str::<Write>(&json).unwrap(),Write::Doc {r#ref:DocRef::Sticky {conversation_id:8},..}));
}
