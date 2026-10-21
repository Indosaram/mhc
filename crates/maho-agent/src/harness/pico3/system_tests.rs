use super::*;
use crate::harness::context::background_context;

fn section()->SystemSection{define_system_section("test".into(),Rc::new(|value|Ok(serde_json::to_string(value)?)))}
#[test]
fn draft_preserves_order_wraps_in_order_and_returns_owned_values()->Result<()>{
    let section=section();let canonical=vec![("test".into(),SectionState{value:JsonValue::Number(1.into()),rendered:"stored".into()})];
    let mut draft=SystemSectionDraft::new(&canonical);let registry=BTreeMap::from([("test".into(),section.clone())]);
    assert_eq!(draft.freeze(&canonical,&registry)?,canonical);
    draft.wrap(&section,Rc::new(|value|Ok(format!("[{value}]"))));draft.wrap(&section,Rc::new(|value|Ok(format!("<{value}>"))));
    let desired=draft.freeze(&canonical,&registry)?;assert_eq!(desired[0].1.rendered,"<[1]>");
    draft.delete("test");draft.set(&section,JsonValue::Number(2.into()));assert_eq!(draft.freeze(&canonical,&registry)?[0].1.rendered,"2");Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn throwing_hook_rolls_back_edits_and_wrappers_before_next_handler()->Result<()>{
    let section=section();let registry=SectionRegistry::default();registry.register(section.clone())?;
    let bad_section=section.clone();let good_section=section.clone();
    let hooks=vec![SystemInstructionsHook{info:HookInfo{kind:"bad".into(),task_id:1,conversation_id:1},handler:Rc::new(move|draft,_,_,_,_|{draft.set(&bad_section,JsonValue::String("bad".into()));draft.wrap(&bad_section,Rc::new(|_|Ok("bad-wrapper".into())));Box::pin(async{Err(Error::InvalidShape("hook failed".into()))})})},SystemInstructionsHook{info:HookInfo{kind:"good".into(),task_id:1,conversation_id:1},handler:Rc::new(move|draft,_,_,_,_|{assert_eq!(draft.get(&good_section),None);draft.set(&good_section,JsonValue::Number(7.into()));Box::pin(async{Ok(None)})})}];
    let settings=PreparationSettings{model:None,thinking_level:serde_json::from_str("\"off\"")?,selected_tools:Vec::new(),profile:"default".into()};let errors=RefCell::new(Vec::new());
    let prepared=prepare_draft(&hooks,RuntimeRegistries { sections:&registry,tools:&ToolRegistry::default() },&Vec::new(),None,&settings,&|error|errors.borrow_mut().push(error),&background_context()).await?;
    assert_eq!(prepared.desired[0].1.rendered,"7");assert_eq!(errors.borrow().len(),1);Ok(())
}
#[test]
fn historical_loadout_removes_then_adds_and_preserves_position()->Result<()>{
    let tool=|name:&str|SystemTool{name:name.into(),description:name.into(),parameters:JsonObject::new()};
    let first=to_stored(&Message::System(SystemMessage{content:String::new(),tools_added:Some(vec![tool("a"),tool("b")]),tools_removed:None,timestamp:1}))?;
    let second=to_stored(&Message::System(SystemMessage{content:String::new(),tools_added:Some(vec![tool("a"),tool("c")]),tools_removed:Some(vec![ToolReference{name:"a".into()}]),timestamp:2}))?;
    assert_eq!(effective_tools(&[first,second])?.iter().map(|tool|tool.name.as_str()).collect::<Vec<_>>(),vec!["b","a","c"]);Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn managed_baseline_metadata_delta_and_head_rebase_use_real_context()->Result<()>{
    use super::super::{session::{Session,CommitOptions},memory::MemoryStorage};
    let session=Session::new(Rc::new(MemoryStorage::new()),Rc::new(RefCell::new(BTreeMap::new())),Rc::new(RefCell::new(BTreeMap::new())),Rc::new(||1))?;
    let ctx=background_context();
    let id=session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async{tx.create_conversation(ConversationSpec::default())}),&ctx,CommitOptions::default()).await?.value;
    let settings=PreparationSettings{model:None,thinking_level:serde_json::from_str("\"off\"")?,selected_tools:Vec::new(),profile:"default".into()};
    let first=PreparationSnapshot{newest_managed:None,newest_baseline:None,newest_head:None,settings,sections_rev:0,tools_rev:0};
    let desired=vec![("test".into(),SectionState{value:JsonValue::Number(1.into()),rendered:"stable".into()})];
    let (baseline,first,desired)=session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{
        let plan=plan_managed_entry(tx,id,&first,&Vec::new(),&desired,&[],1).await?.ok_or_else(||Error::InvalidShape("missing baseline".into()))?;
        assert!(plan.data.baseline.is_some());assert_eq!(plan.model.len(),1);
        let data=from_json(&to_stored(&plan.data)?.as_json().clone())?;
        let baseline=tx.append_entry(id,NewEntry{kind:"pi.system".into(),model:Some(plan.model),data:Some(data),head:None,edits:plan.edits})?;Ok((baseline,first,desired))
    }),&ctx,CommitOptions::default()).await?.value;
    session.commit(Invoker::Kernel{conversation_id:Some(id)},|tx,_|Box::pin(async move{
        let folded=fold_canonical(tx,id).await?;assert_eq!(folded.newest_baseline,Some(baseline));assert_eq!(folded.canonical,desired);
        let mut snapshot=PreparationSnapshot{newest_managed:Some(baseline),newest_baseline:Some(baseline),..first};
        let changed=vec![("test".into(),SectionState{value:JsonValue::Number(2.into()),rendered:"stable".into()})];
        let delta=plan_managed_entry(tx,id,&snapshot,&desired,&changed,&[],2).await?.ok_or_else(||Error::InvalidShape("missing metadata delta".into()))?;
        assert!(delta.model.is_empty());assert!(delta.data.baseline.is_none());
        snapshot.newest_head=Some(baseline+1);
        let rebase=plan_managed_entry(tx,id,&snapshot,&desired,&changed,&[],3).await?.ok_or_else(||Error::InvalidShape("missing rebase".into()))?;assert!(rebase.data.baseline.is_some());Ok(())
    }),&ctx,CommitOptions::default()).await?;Ok(())
}
