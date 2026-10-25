//! Durable child reports, fenced by run epoch and generation.
use crate::lifecycle_record::{read_lifecycle_state, read_waits};
use indexmap::IndexMap;
use serde_json::{Value, json};
pub struct LifecycleChildren {
    records: IndexMap<String, Value>,
}
fn read_child_report(v: &Value) -> Option<Value> {
    let state = read_lifecycle_state(v.get("state")?)?;
    let epoch = v.get("runEpoch")?.as_u64()?;
    let generation = v.get("generation")?.as_u64()?;
    if epoch > 9_007_199_254_740_991 || generation > 9_007_199_254_740_991 {
        return None;
    }
    let mut r = json!({"taskId":v.get("taskId")?.as_str()?,"childSessionId":v.get("childSessionId")?.as_str()?,"runEpoch":epoch,"generation":generation,"state":state});
    if let Some(mut w) = read_waits(&v["waitingOn"]) {
        w["human"] = json!([]);
        r["waitingOn"] = w;
    }
    if ["completed", "error", "lost", "interrupted", "cancelled"]
        .contains(&v["terminal"].as_str().unwrap_or(""))
    {
        r["terminal"] = v["terminal"].clone();
    }
    Some(r)
}
impl LifecycleChildren {
    pub fn new(entries: &[Value]) -> Self {
        let mut s = Self {
            records: IndexMap::new(),
        };
        for e in entries {
            if e["type"] == "custom" && e["customType"] == "child-lifecycle"
                && let Some(r) = read_child_report(&e["data"]) {
                    let _ = s.accept(r, |_| Ok(()));
                }
        }
        s
    }
    pub fn reports(&self) -> Vec<Value> {
        self.records.values().cloned().collect()
    }
    pub fn active_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.get("terminal").is_none())
            .count()
    }
    pub fn report(
        &mut self,
        report: &Value,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) -> Result<bool, String> {
        let r = read_child_report(report).ok_or("Invalid child lifecycle report")?;
        self.accept(r, append)
    }
    fn accept(
        &mut self,
        r: Value,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) -> Result<bool, String> {
        let key = r["taskId"].as_str().ok_or("missing task id")?.to_owned();
        if let Some(p) = self.records.get(&key) {
            let epoch = r["runEpoch"].as_u64();
            let previous = p["runEpoch"].as_u64();
            if epoch < previous
                || (epoch == previous
                    && (p.get("terminal").is_some()
                        || r["generation"].as_u64() <= p["generation"].as_u64()))
            {
                return Ok(false);
            }
        }
        append(&r)?;
        self.records.insert(key, r);
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_is_final_per_epoch() {
        let mut c = LifecycleChildren::new(&[]);
        let mut r = json!({"taskId":"t","childSessionId":"s","runEpoch":1,"generation":1,"state":"done","terminal":"completed"});
        assert!(c.report(&r, |_| Ok(())).expect("append"));
        r["generation"] = json!(2);
        assert!(!c.report(&r, |_| Ok(())).expect("append"));
        r["runEpoch"] = json!(2);
        r.as_object_mut().expect("object").remove("terminal");
        assert!(c.report(&r, |_| Ok(())).expect("append"));
        assert_eq!(c.active_count(), 1);
    }
}
