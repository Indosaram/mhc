use maho_chord::{context::BACKGROUND_CONTEXT, services::handle::*, types::*};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn object(receiver: &str) -> Arc<ServiceObject> {
    Arc::new(ServiceObject { receiver: Arc::new(receiver.to_owned()), members: BTreeMap::from([("call".into(), ServiceValue::Method(Arc::new(|receiver, _, _| Box::pin(async move {
        let value = receiver.downcast_ref::<String>().ok_or("wrong receiver")?;
        Ok(Some(json!(value).into()))
    }))))]) })
}
#[tokio::test]
async fn cached_method_resolves_current_receiver_and_checks_access() -> Result<(), ServiceError> {
    let slot = ServiceSlot::new(&"test".into(), true);
    slot.bind(object("first"));
    let view = slot.view(Arc::new(|| Ok(())));
    let method = view.member(&"call".into())?;
    assert_eq!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(json!("first").into()));
    slot.bind(object("second"));
    assert!(Arc::ptr_eq(&method, &view.member(&"call".into())?));
    assert_eq!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(json!("second").into()));
    slot.unbind();
    assert!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    Ok(())
}
#[tokio::test]
async fn nested_cached_methods_keep_the_nested_receiver_and_guard() -> Result<(), ServiceError> {
    let slot = ServiceSlot::new(&"test".into(), true);
    slot.bind(Arc::new(ServiceObject { receiver: Arc::new(()), members: BTreeMap::from([("nested".into(), ServiceValue::Object(object("nested-first")))]) }));
    let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true)); let flag = allowed.clone();
    let view = slot.view(Arc::new(move || { if flag.load(std::sync::atomic::Ordering::Acquire) { Ok(()) } else { Err("closed".into()) } }));
    let nested = view.member(&"nested".into())?; let method = nested.child(&"call".into())?;
    assert!(Arc::ptr_eq(&method, &nested.child(&"call".into())?));
    assert_eq!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(json!("nested-first").into()));
    slot.bind(Arc::new(ServiceObject { receiver: Arc::new(()), members: BTreeMap::from([("nested".into(), ServiceValue::Object(object("nested-second")))]) }));
    assert_eq!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(json!("nested-second").into()));
    allowed.store(false, std::sync::atomic::Ordering::Release);
    assert!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err()); Ok(())
}

#[tokio::test]
async fn cached_callable_rejects_nonmethods_and_recovers_after_replacement() -> Result<(), ServiceError> {
    let slot = ServiceSlot::new(&"test".into(), true); slot.bind(object("first"));
    let view = slot.view(Arc::new(|| Ok(()))); let call = view.member(&"call".into())?;
    slot.bind(Arc::new(ServiceObject { receiver: Arc::new(()), members: BTreeMap::from([("call".into(), ServiceValue::Json(JsonValue::Null))]) }));
    assert!(call.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    slot.bind(object("last"));
    assert!(Arc::ptr_eq(&call, &view.member(&"call".into())?));
    assert_eq!(call.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?, Some(json!("last").into()));
    Ok(())
}

#[tokio::test]
async fn originally_noncallable_view_stays_noncallable_after_replacement() -> Result<(), ServiceError> {
    let slot = ServiceSlot::new(&"test".into(), true);
    slot.bind(Arc::new(ServiceObject { receiver: Arc::new(()), members: BTreeMap::from([("call".into(), ServiceValue::Object(object("nested")))]) }));
    let view = slot.view(Arc::new(|| Ok(()))); let noncallable = view.member(&"call".into())?;
    slot.bind(object("now a method"));
    assert!(noncallable.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    Ok(())
}
