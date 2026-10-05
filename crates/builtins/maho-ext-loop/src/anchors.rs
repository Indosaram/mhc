use crate::{index::LOOP_TICK_ENTRY_TYPE,types::SentinelDeliveryState};
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
}
