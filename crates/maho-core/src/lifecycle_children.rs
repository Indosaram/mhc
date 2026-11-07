//! Durable child reports, fenced by run epoch and generation.
use crate::lifecycle_attention::read_human_wait;
use crate::lifecycle_record::read_lifecycle_state;
use crate::lifecycle_wait_items::read_wait_items;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

const MAX_CHILD_REPORTS: usize = 32;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn safe_non_negative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number)).then_some(number as u64)
}

fn valid_since(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|since| chrono::DateTime::parse_from_rfc3339(since).is_ok())
}

/// A wait source with its counts and named items.
fn read_source(source: &Value, with_retry: bool) -> Option<Value> {
    let mut item = Map::new();
    item.insert("source".into(), json!(source.get("source")?.as_str()?));
    item.insert(
        "activeCount".into(),
        Value::Number(source.get("activeCount")?.as_number()?.clone()),
    );
    let named = read_wait_items(source.get("items"), source.get("omittedItems"));
    if let Some(items) = named.items {
        item.insert("items".into(), json!(items));
    }
    if let Some(omitted) = named.omitted_items {
        item.insert("omittedItems".into(), json!(omitted));
    }
    if with_retry && source["retryAtMs"].is_number() {
        item.insert("retryAtMs".into(), source["retryAtMs"].clone());
    }
    Some(Value::Object(item))
}

/// senpi `readWaits`: the one reader of a durable or reported `waitingOn`; named detail never invalidates the
/// counts.
pub fn read_waits(v: &Value) -> Option<Value> {
    let children = v.get("children")?.as_number()?;
    let background: Vec<Value> = v["background"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|source| read_source(source, true))
        .collect();
    let human: Vec<Value> = v["human"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(read_human_wait)
        .collect();
    let child_sources: Vec<Value> = v["childSources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|source| read_source(source, false))
        .collect();
    let mut reports: Vec<Value> = v["childReports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|report| {
            let state = read_lifecycle_state(report.get("state")?)?;
            let mut item = Map::new();
            item.insert("taskId".into(), json!(report.get("taskId")?.as_str()?));
            item.insert(
                "childSessionId".into(),
                json!(report.get("childSessionId")?.as_str()?),
            );
            item.insert(
                "runEpoch".into(),
                json!(safe_non_negative_integer(report.get("runEpoch")?)?),
            );
            item.insert("state".into(), json!(state));
            if let Some(label) = report.get("label").and_then(Value::as_str) {
                item.insert("label".into(), json!(label));
            }
            if let Some(since) = report.get("since").and_then(valid_since) {
                item.insert("since".into(), json!(since));
            }
            Some(Value::Object(item))
        })
        .collect();
    reports.sort_by(|a, b| a["taskId"].as_str().cmp(&b["taskId"].as_str()));
    let omitted = reports.len().saturating_sub(MAX_CHILD_REPORTS) as u64
        + v.get("childReportsOmitted")
            .and_then(Value::as_f64)
            .filter(|n| n.fract() == 0.0 && *n > 0.0 && *n <= MAX_SAFE_INTEGER)
            .map_or(0, |n| n as u64);
    reports.truncate(MAX_CHILD_REPORTS);
    let mut waits = json!({"children": children, "background": background, "human": human});
    if !child_sources.is_empty() {
        waits["childSources"] = json!(child_sources);
    }
    if !reports.is_empty() {
        waits["childReports"] = json!(reports);
    }
    if omitted > 0 {
        waits["childReportsOmitted"] = json!(omitted);
    }
    Some(waits)
}
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
    // A child report keeps its label and a valid `since` (senpi#4106).
    if let Some(label) = v.get("label").and_then(Value::as_str) {
        r["label"] = json!(label);
    }
    if let Some(since) = v.get("since").and_then(valid_since) {
        r["since"] = json!(since);
    }
    if let Some(w) = read_waits(&v["waitingOn"]) {
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
            if e["type"] == "custom"
                && e["customType"] == "child-lifecycle"
                && let Some(r) = read_child_report(&e["data"])
            {
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

    /// senpi test/suite/regressions/4106-child-waits.test.ts: child reports keep label and since; waits carry
    /// sorted, capped child reports and items.
    #[test]
    fn waits_read_child_sources_reports_and_items() {
        let waits = read_waits(&json!({
            "children": 1,
            "background": [{"source": "monitor", "activeCount": 1, "items": [{"id": "m", "label": "tail"}]}],
            "childSources": [{"source": "task", "activeCount": 2}],
            "childReports": [
                {"taskId": "b", "childSessionId": "s2", "runEpoch": 1, "state": "running", "label": "B", "since": "bogus"},
                {"taskId": "a", "childSessionId": "s1", "runEpoch": 1, "state": "running", "since": "2026-01-01T00:00:00.000Z"},
                {"taskId": "c", "childSessionId": "s3", "runEpoch": -1, "state": "running"}
            ],
            "childReportsOmitted": 2
        }))
        .expect("waits");
        assert_eq!(
            waits["background"][0]["items"][0],
            json!({"id": "m", "label": "tail"})
        );
        assert_eq!(waits["childSources"][0]["activeCount"], 2);
        assert_eq!(waits["childReports"][0]["taskId"], "a");
        assert_eq!(waits["childReports"][1]["label"], "B");
        assert!(waits["childReports"][1].get("since").is_none());
        assert_eq!(waits["childReports"].as_array().map(Vec::len), Some(2));
        assert_eq!(waits["childReportsOmitted"], 2);
        let report = read_child_report(&json!({"taskId": "t", "childSessionId": "s", "runEpoch": 1, "generation": 1, "state": "running", "label": "L"})).expect("report");
        assert_eq!(report["label"], "L");
    }
}
