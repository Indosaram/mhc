use super::*;
use maho_chord::delta::track;

fn json(text:&str)->JsonValue { serde_json::from_str(text).unwrap() }
#[test]
fn nested_identity_and_mutations_share_revocation() {
    let tracker=track(json(r#"{"a":{"x":1},"b":{"x":1},"list":[{"n":1}]}"#)).unwrap();
    let mut change=tracker.begin_change();
    let membrane=Membrane::new("test");
    let root=membrane.wrap(change.state().unwrap()).unwrap();
    let a=root.get("a").unwrap().unwrap(); let again=root.get("a").unwrap().unwrap();
    let b=root.get("b").unwrap().unwrap();
    assert!(a.same_identity(&again).unwrap()); assert!(!a.same_identity(&b).unwrap());
    a.set("x",&json("2")).unwrap();
    let mut escaped=None;
    root.get("list").unwrap().unwrap().for_each(|item,_,array| {
        escaped=Some((item,array)); Ok(())
    }).unwrap();
    assert!(!change.prepare().unwrap().ops().is_empty());
    membrane.revoke();
    assert!(root.value().is_err()); assert!(a.get("x").is_err());
    let (item,array)=escaped.unwrap(); assert!(item.value().is_err()); assert!(array.len().is_err());
    assert!(root.keys().is_err()); assert!(root.delete("a").is_err());
    assert!(root.is_extensible().is_err()); assert!(root.same_identity(&root).is_err());
}
#[test]
fn wrappers_and_cycles_reject_before_partial_mutation() {
    let tracker=track(json(r#"{"x":null}"#)).unwrap();
    let change=tracker.begin_change(); let membrane=Membrane::new("test");
    let root=membrane.wrap(change.state().unwrap()).unwrap();
    let wrapper=PlainInput::Object(vec![("nested".into(),PlainInput::Wrapper(root.clone()))]);
    assert!(matches!(root.set_input("x",&wrapper),Err(Error::WrapperAssignment(_))));
    let cycle=Rc::new(RefCell::new(PlainInput::Json(JsonValue::Null)));
    *cycle.borrow_mut()=PlainInput::Array(vec![PlainInput::Shared(cycle.clone())]);
    assert!(matches!(root.set_input("x",&PlainInput::Shared(cycle.clone())),Err(Error::CyclicAssignment(_))));
    *cycle.borrow_mut()=PlainInput::Json(JsonValue::Null);
    assert_eq!(root.get("x").unwrap().unwrap().value().unwrap(),JsonValue::Null);
    let shared=Rc::new(RefCell::new(PlainInput::Json(json(r#"{"n":1}"#))));
    root.set_input("x",&PlainInput::Array(vec![PlainInput::Shared(shared.clone()),PlainInput::Shared(shared)])).unwrap();
    assert_eq!(root.get("x").unwrap().unwrap().len().unwrap(),2);
}
#[test]
fn drop_revokes_even_on_callback_failure_and_rejects_reflection_mutation() {
    let tracker=track(json(r#"{"list":[1,2]}"#)).unwrap(); let change=tracker.begin_change();
    let retained={
        let membrane=Membrane::new("failure"); let root=membrane.wrap(change.state().unwrap()).unwrap();
        assert!(root.define_accessor().is_err()); assert!(root.set_prototype().is_err());
        assert!(root.prevent_extensions().is_err()); assert!(root.set("__proto__",&JsonValue::Null).is_err());
        let list=root.get("list").unwrap().unwrap();
        assert!(list.for_each(|_,_,_|Err(Error::Closed)).is_err()); list
    };
    assert!(matches!(retained.pop(),Err(Error::Revoked(_))));
}
#[test]
fn array_results_are_revocable_and_plain_values_are_copied() {
    let tracker=track(json(r#"{"list":[1,2,3]}"#)).unwrap(); let change=tracker.begin_change();
    let membrane=Membrane::new("arrays"); let root=membrane.wrap(change.state().unwrap()).unwrap();
    let list=root.get("list").unwrap().unwrap();
    let removed=list.splice(1,1,&[json("4")]).unwrap();
    assert_eq!(removed[0].value().unwrap(),json("2"));
    list.reverse().unwrap(); assert_eq!(list.value().unwrap(),json("[3,4,1]"));
    membrane.revoke(); assert!(removed[0].value().is_err());
}
