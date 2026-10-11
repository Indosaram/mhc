use crate::{index::LOOP_TICK_ENTRY_TYPE,types::SentinelDeliveryState};
/// Compaction-aware session entries for anchor presence, matching upstream
/// `anchorPresent(ctx, ..)` which reads `ctx.sessionManager.buildContextEntries()`
/// (`loop/index.ts:287`): entries dropped by an accepted compaction no longer anchor a reminder.
pub fn compaction_aware_entries(context:&maho_ext_api::ExtensionContext)->Vec<maho_ext_api::SessionEntry> {
    let entries=context.session_manager.get_branch();
    let values:Vec<serde_json::Value>=entries.iter().map(|entry|entry.data.clone()).collect();
    let surviving=maho_core::session_manager::build_context_entries(&values,context.session_manager.get_leaf_id().as_deref());
    let ids:std::collections::BTreeSet<&str>=surviving.iter().filter_map(|value|value.get("id").and_then(serde_json::Value::as_str)).collect();
    entries.into_iter().filter(|entry|ids.contains(entry.id.as_str())).collect()
}
pub fn anchor_present(entries:&[maho_ext_api::SessionEntry],loop_id:&str,delivery_id:Option<&str>)->bool {
    entries.iter().any(|entry| {
        if entry.data.get("customType").and_then(serde_json::Value::as_str)!=Some(LOOP_TICK_ENTRY_TYPE) { return false; }
        let payload=match entry.kind.as_str() { "custom"=>entry.data.get("data"),"custom_message"=>entry.data.get("details"),_=>None };
        let Some(payload)=payload else { return false; };
        if payload.get("loopId").is_some_and(|id|id!=loop_id) { return false; }
        delivery_id.is_none_or(|id|payload.get("deliveryId").and_then(serde_json::Value::as_str)==Some(id))
    })
}
pub fn restored_delivery_state(state:&SentinelDeliveryState,entries:&[maho_ext_api::SessionEntry],loop_id:&str)->SentinelDeliveryState {
    let anchor=state.last_loop_file_delivered.as_ref().map(|file|file.anchor_delivery_id.as_str());
    let needs_full=(state.autonomous_preamble_delivered||state.last_loop_file_delivered.is_some())&&!anchor_present(entries,loop_id,anchor);
    SentinelDeliveryState { force_full_delivery:state.force_full_delivery||needs_full,..state.clone() }
}
#[cfg(test)] mod tests {
    use super::*;
    fn entry(kind:&str,key:&str,loop_id:&str,delivery:&str)->maho_ext_api::SessionEntry {
        maho_ext_api::SessionEntry { id:"e".into(),parent_id:None,timestamp:String::new(),kind:kind.into(),data:serde_json::json!({"customType":"loop-tick",key:{"loopId":loop_id,"deliveryId":delivery}}) }
    }
    #[test] fn both_entry_kinds_can_anchor_but_wrong_loop_or_delivery_cannot() {
        for (kind,key) in [("custom","data"),("custom_message","details")] {
            let entries=vec![entry(kind,key,"a","d")]; assert!(anchor_present(&entries,"a",Some("d")));
            assert!(!anchor_present(&entries,"b",Some("d"))); assert!(!anchor_present(&entries,"a",Some("other")));
        }
    }
    #[test] fn missing_autonomous_anchor_forces_full_but_fresh_state_does_not() {
        let state=SentinelDeliveryState { autonomous_preamble_delivered:true,..Default::default() };
        assert!(restored_delivery_state(&state,&[],"a").force_full_delivery);
        assert!(!restored_delivery_state(&state,&[entry("custom","data","a","d")],"a").force_full_delivery);
        assert!(!restored_delivery_state(&Default::default(),&[],"a").force_full_delivery);
    }
    #[test] fn malformed_loop_id_cannot_anchor_another_loops_preamble() {
        for id in [serde_json::Value::Null,serde_json::json!(12),serde_json::json!(false)] {
            let mut candidate=entry("custom","data","a","d"); candidate.data["data"]["loopId"]=id;
            assert!(!anchor_present(&[candidate],"a",None));
        }
        let mut legacy=entry("custom","data","a","d"); legacy.data["data"].as_object_mut().unwrap().remove("loopId");
        assert!(anchor_present(&[legacy],"a",None));
    }
    struct BranchSession(Vec<maho_ext_api::SessionEntry>);
    impl maho_ext_api::ToolSessionManager for BranchSession { fn session_id(&self)->&str { "s" } fn session_file(&self)->Option<&std::path::Path> { None } }
    impl maho_ext_api::SessionManager for BranchSession {
        fn get_entries(&self)->Vec<maho_ext_api::SessionEntry> { self.0.clone() }
        fn get_branch(&self)->Vec<maho_ext_api::SessionEntry> { self.0.clone() }
        fn get_leaf_id(&self)->Option<String> { self.0.last().map(|entry|entry.id.clone()) }
        fn get_session_name(&self)->Option<String> { None }
    }
    fn value_entry(id:&str,parent:Option<&str>,kind:&str)->maho_ext_api::SessionEntry {
        let mut data=serde_json::json!({"id":id,"type":kind});
        if let Some(parent)=parent { data["parentId"]=parent.into(); }
        maho_ext_api::SessionEntry { id:id.into(),parent_id:parent.map(str::to_owned),timestamp:String::new(),kind:kind.into(),data }
    }
    #[test] fn compaction_drops_entries_before_the_first_kept_id() {
        let mut compaction=value_entry("compaction",Some("msg1"),"compaction"); compaction.data["firstKeptEntryId"]="msg2".into();
        let branch=BranchSession(vec![value_entry("msg1",None,"message"),compaction,value_entry("msg2",Some("compaction"),"message"),value_entry("tick",Some("msg2"),"custom")]);
        let mut context=crate::controller_tests::context(); context.session_manager=std::sync::Arc::new(branch);
        let entries=compaction_aware_entries(&context);
        let ids=entries.iter().map(|entry|entry.id.as_str()).collect::<Vec<_>>();
        assert!(!ids.contains(&"msg1"),"compacted-away entry must not survive: {ids:?}");
        assert!(ids.contains(&"compaction")&&ids.contains(&"msg2")&&ids.contains(&"tick"));
    }
    #[test] fn no_compaction_keeps_the_whole_branch() {
        let branch=BranchSession(vec![value_entry("a",None,"message"),value_entry("b",Some("a"),"custom")]);
        let mut context=crate::controller_tests::context(); context.session_manager=std::sync::Arc::new(branch);
        assert_eq!(compaction_aware_entries(&context).iter().map(|entry|entry.id.clone()).collect::<Vec<_>>(),vec!["a".to_owned(),"b".to_owned()]);
    }
}
