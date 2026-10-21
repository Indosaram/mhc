use super::*;
use super::super::memory::MemoryStorage;
use crate::harness::context::background_context;

fn entry(kind:&str)->NewEntry{NewEntry{kind:kind.into(),model:None,data:None,head:None,edits:None}}
pub(super) fn config()->KindConfig{
    let mut rewindable=JsonObject::new();
    for(key,value)in [("thinkingLevel",JsonValue::String("off".into())),("selectedTools",JsonValue::array(Vec::new())),("profile",JsonValue::String("default".into())),("threshold",JsonValue::Number(100.into())),("keepRecent",JsonValue::Number(1.into()))]{rewindable.insert(key.into(),value);}
    let mut sticky=JsonObject::new();sticky.insert("retry".into(),JsonValue::object(vec![("enabled".into(),JsonValue::Bool(false)),("maxRetries".into(),JsonValue::Number(0.into())),("baseDelayMs".into(),JsonValue::Number(1.into()))]));sticky.insert("steeringMode".into(),JsonValue::String("all".into()));sticky.insert("followUpMode".into(),JsonValue::String("all".into()));KindConfig{rewindable,sticky}
}
pub(super) fn generation()->Rc<ExecutableKind>{
    Rc::new(ExecutableKind{token:KindToken::core("pi.generation"),turn:true,config:config(),slot:None,describe:None,initial:Rc::new(|_,_,_,_|Box::pin(async{Ok(super::super::scheduler::Step::Done(Box::new(|_,_,_|Box::pin(async{Ok(Completion::Completed{result:JsonValue::Null})}))))})),phases:BTreeMap::new(),inflight:BTreeSet::new(),abort:Rc::new(|_,_,_,_|Box::pin(async{let closure:super::super::scheduler::AbortClosure=Box::new(|_,_,_|Box::pin(async{Ok(JsonValue::Null)}));Ok(closure)}))})
}
async fn session()->Result<(Rc<Session>,Id)>{
    let kinds=Rc::new(RefCell::new(BTreeMap::from([("pi.generation".into(),generation())])));
    let session=Session::new(Rc::new(MemoryStorage::new()),kinds,Rc::new(RefCell::new(BTreeMap::new())),Rc::new(||42))?;
    let id=session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async{tx.create_conversation(ConversationSpec::default())}),&background_context(),CommitOptions::default()).await?.value;Ok((session,id))
}
#[tokio::test(flavor="current_thread")]
async fn caught_scan_error_poison_rolls_back_records_and_documents()->Result<()>{
    let(session,id)=session().await?;let ctx=background_context();
    let result=session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{
        tx.config_set(id,"profile",JsonValue::String("discard".into()))?;
        tx.append_entry(id,entry("discard"))?;
        assert!(matches!(tx.scan_entries(EntryScan{conversation_id:id,kind:None,with_head:None,before:None,limit:10}).await,Err(Error::ReadAfterWrite{..})));
        tx.append_entry(id,entry("also-discard"))?;Ok(())
    }),&ctx,CommitOptions{docs:vec![DocRef::Rewindable{conversation_id:id}],closing:false}).await;
    assert!(matches!(result,Err(Error::ReadAfterWrite{..})));
    assert!(session.storage.scan_entries(EntryScan{conversation_id:id,kind:None,with_head:None,before:None,limit:10},&ctx).await?.is_empty());
    assert_eq!(session.storage.doc(DocRef::Rewindable{conversation_id:id},&ctx).await?.and_then(|value|value.get("profile").cloned()),Some(JsonValue::String("default".into())));Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn direct_reads_observe_overlays_and_handles_revoke()->Result<()>{
    let(session,id)=session().await?;
    let result=session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{
        let input=tx.write(id,entry("new")).await?;let input=tx.input(input).await?.ok_or_else(||Error::InvalidShape("missing overlay input".into()))?;
        assert!(tx.entry(input.entry.ok_or_else(||Error::InvalidShape("missing overlay entry".into()))?).await?.is_some());
        let doc=tx.rewindable(id)?;doc.set("profile",&JsonValue::String("changed".into()))?;
        assert!(matches!(tx.config_get(id,"profile")?,Some(JsonValue::String(value)) if value.as_str()==Some("changed")));Ok(doc)
    }),&background_context(),CommitOptions{docs:vec![DocRef::Rewindable{conversation_id:id},DocRef::Sticky{conversation_id:id}],closing:false}).await?;
    assert!(matches!(result.value.value(),Err(Error::Revoked(_))));Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn dedup_precedes_busy_rejection_and_forks_use_commit_history()->Result<()>{
    let(session,id)=session().await?;let ctx=background_context();
    let content:UserInput=serde_json::from_str("\"hello\"")?;
    let first=session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{let send=||SendInput{content:content.clone(),request_id:Some("request".into()),when_busy:Some(WhenBusy::Reject)};let first=tx.send(id,send()).await?;let again=tx.send(id,send()).await?;assert_eq!(first,again);Ok(first)}),&ctx,CommitOptions{docs:vec![DocRef::Rewindable{conversation_id:id},DocRef::Sticky{conversation_id:id}],closing:false}).await?.value;
    let input=session.storage.input(first,&ctx).await?.ok_or_else(||Error::InvalidShape("missing input".into()))?;
    let at=input.entry.ok_or_else(||Error::InvalidShape("missing entry".into()))?;
    let child=session.fork(id,Some(at),ConversationSpec::default(),&ctx).await?;
    assert_eq!(session.storage.conversation(child,&ctx).await?.and_then(|c|c.parent).map(|p|p.at),Some(at));
    assert!(session.storage.doc(DocRef::Sticky{conversation_id:child},&ctx).await?.is_some());Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn line_listener_sees_committed_cache_before_offline_listener()->Result<()>{
    let(session,id)=session().await?;let order=Rc::new(RefCell::new(Vec::new()));let line_order=order.clone();let weak=Rc::downgrade(&session);
    session.subscribe_line(Rc::new(move|_|{let session=weak.upgrade().ok_or(Error::Closed)?;assert_eq!(session.loaded_document(&DocRef::Rewindable{conversation_id:id})?.get("profile"),Some(&JsonValue::String("published".into())));line_order.borrow_mut().push("line");Ok(())}));
    let offline_order=order.clone();session.subscribe(Rc::new(move|_|{assert_eq!(*offline_order.borrow(),vec!["line"]);offline_order.borrow_mut().push("offline");Ok(())}));
    session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{tx.config_set(id,"profile",JsonValue::String("published".into()))}),&background_context(),CommitOptions::default()).await?;
    assert_eq!(*order.borrow(),vec!["line","offline"]);Ok(())
}
