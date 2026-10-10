use super::types::{WorkpoolItem, WorkpoolItemStatus, WorkpoolYieldValue};
use super::yield_event::yield_digest;

pub type YieldReceipt = Result<YieldAccepted, YieldRefusal>;
#[derive(Debug, Clone, PartialEq, Eq)] pub struct YieldAccepted { pub key: String, pub item_id: String }
#[derive(Debug, Clone, PartialEq, Eq)] pub struct YieldRefusal { pub key: Option<String>, pub code: String, pub message: String }

pub fn apply_yield(mut item: WorkpoolItem, result: &WorkpoolYieldValue) -> WorkpoolItem {
    item.data = None; item.error = None; item.yield_sha256 = Some(yield_digest(result));
    match result { WorkpoolYieldValue::Data { data, .. } => { item.status=WorkpoolItemStatus::Completed; item.data=Some(data.clone()); }, WorkpoolYieldValue::Error { error, .. } => { item.status=WorkpoolItemStatus::Error; item.error=Some(error.clone()); } }
    item
}

pub fn refuse_yield(code: impl Into<String>, message: impl Into<String>, key: Option<String>) -> YieldRefusal { YieldRefusal { key, code: code.into(), message: message.into() } }

#[cfg(test)]
mod tests { use super::*; #[test] fn data_yield_completes_item() { let item=WorkpoolItem{key:"a".into(),input:serde_json::Value::Null,item_id:"i".into(),status:WorkpoolItemStatus::Assigned,binding:None,delivery:None,data:None,error:None,yield_sha256:None}; let out=apply_yield(item,&WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(2)}); assert_eq!(out.status,WorkpoolItemStatus::Completed); assert_eq!(out.data,Some(serde_json::json!(2))); } }
