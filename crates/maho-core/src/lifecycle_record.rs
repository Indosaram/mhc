//! Validation of durable lifecycle records; unknown/private JSON never bypasses validation.
use crate::lifecycle_attention::read_human_wait;
use serde_json::{Value, json};
pub fn read_lifecycle_state(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| {
        [
            "running",
            "waiting_children",
            "waiting_background",
            "waiting_human",
            "paused",
            "held",
            "done",
            "failed",
            "cancelled",
        ]
        .contains(s)
    })
}
/// The one reader of a durable or reported `waitingOn` lives in `lifecycle_children` (senpi#4106).
pub use crate::lifecycle_children::read_waits;
pub fn read_holds(v: &Value) -> Option<Value> {
    Some(json!(
        v.as_array()?
            .iter()
            .filter_map(|h| {
                let kind = h.get("kind")?.as_str()?;
                if !["session_hold", "run_fence", "inherited_hold", "unattended"].contains(&kind) {
                    return None;
                }
                Some(json!({"kind":kind,"since":h.get("since")?.as_str()?}))
            })
            .collect::<Vec<_>>()
    ))
}
pub fn read_lifecycle_record(v: &Value) -> Option<Value> {
    let state = read_lifecycle_state(v.get("state")?)?;
    for k in ["sessionId", "since", "rootSessionId"] {
        v.get(k)?.as_str()?;
    }
    for k in ["seq", "epoch", "humanInputRevision"] {
        v.get(k)?.as_number()?;
    }
    for k in ["selfSettleEligible", "runPaused"] {
        v.get(k)?.as_bool()?;
    }
    if !["human", "automatic"].contains(&v["epochOrigin"].as_str()?)
        || !["root", "child"].contains(&v["role"].as_str()?)
    {
        return None;
    }
    let mut r = json!({"state":state,"restoring":true,"attentionSeq":v["attentionSeq"].as_number().cloned().unwrap_or_else(||0.into())});
    for k in [
        "sessionId",
        "since",
        "rootSessionId",
        "seq",
        "epoch",
        "humanInputRevision",
        "selfSettleEligible",
        "runPaused",
        "epochOrigin",
        "role",
    ] {
        r[k] = v[k].clone();
    }
    for k in ["persisted", "epochSealed", "runActive", "interrupted"] {
        r[k] = json!(v[k] == true);
    }
    for k in [
        "completedGoalId",
        "completedRunId",
        "completionId",
        "parentSessionId",
        "taskId",
        "runId",
    ] {
        if v[k].is_string() {
            r[k] = v[k].clone();
        }
    }
    r["synthetic"] = json!(
        v["synthetic"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some(
                json!({"key":s.get("key")?.as_str()?,"wait":read_human_wait(&s["wait"])?})
            ))
            .collect::<Vec<_>>()
    );
    if let Some(w) = read_waits(&v["waitingOn"]) {
        r["waitingOn"] = w;
    }
    if let Some(h) = read_holds(&v["holds"]) {
        r["holds"] = h;
    }
    if v["settleDecision"]["decisionId"].is_string() {
        r["settleDecision"] = json!({"decisionId":v["settleDecision"]["decisionId"]});
    }
    if v["settleRequest"]["epoch"].is_number() && v["settleRequest"]["revision"].is_number() {
        r["settleRequest"] =
            json!({"epoch":v["settleRequest"]["epoch"],"revision":v["settleRequest"]["revision"]});
    }
    if ["goal", "run", "agent", "reply"].contains(&v["completion"]["kind"].as_str().unwrap_or(""))
        && v["completion"]["at"].is_string()
    {
        let mut c = json!({"kind":v["completion"]["kind"],"at":v["completion"]["at"]});
        for k in ["goalId", "runId"] {
            if v["completion"][k].is_string() {
                c[k] = v["completion"][k].clone();
            }
        }
        r["completion"] = c;
    }
    if [
        "provider_error",
        "tool_error",
        "task_error",
        "lost",
        "interrupted",
    ]
    .contains(&v["failure"]["reason"].as_str().unwrap_or(""))
    {
        let mut f = json!({"reason":v["failure"]["reason"]});
        for k in ["message", "taskId"] {
            if v["failure"][k].is_string() {
                f[k] = v["failure"][k].clone();
            }
        }
        for k in ["retryAtMs", "runEpoch"] {
            if v["failure"][k].is_number() {
                f[k] = v["failure"][k].clone();
            }
        }
        r["failure"] = f;
    }
    if let Some(stop) = crate::lifecycle_stop::read_last_stop(&v["lastStop"], &v["runId"]) {
        r["lastStop"] = json!(stop);
    }
    let t = &v["lastTurnEnd"];
    if [
        "clean",
        "user-abort",
        "system-abort",
        "provider-abort",
        "unknown-abort",
        "provider-error",
        "policy-rejection",
    ]
    .contains(&t["endedBy"].as_str().unwrap_or(""))
        && t["deadlineStop"].is_boolean()
        && t["budgetSpent"].is_boolean()
    {
        let mut end = json!({"endedBy":t["endedBy"],"deadlineStop":t["deadlineStop"],"budgetSpent":t["budgetSpent"]});
        if t["providerRetryAtMs"].is_number() {
            end["providerRetryAtMs"] = t["providerRetryAtMs"].clone();
        }
        r["lastTurnEnd"] = end;
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({
            "state":"waiting_children","sessionId":"s","since":"2026-10-27T00:00:00.000Z","rootSessionId":"s","seq":3,
            "epoch":2,"humanInputRevision":0,"selfSettleEligible":true,"runPaused":false,"epochOrigin":"human",
            "role":"root","runId":"r1",
            "waitingOn":{"children":1,"background":[],"human":[],
                "childSources":[{"source":"task","activeCount":1,"items":[{"id":"c1","label":"build"}]}],
                "childReports":[{"taskId":"t","childSessionId":"c","runEpoch":1,"state":"running"}]},
            "lastStop":{"cause":"no_wake","runId":"r1","at":"2026-10-27T00:00:01.000Z","epoch":2}
        })
    }

    /// senpi#3864 / #4106: a restored record keeps its run's lastStop and its named child waits.
    #[test]
    fn restore_reads_last_stop_child_sources_and_child_reports() {
        let r = read_lifecycle_record(&record()).expect("valid record");
        assert_eq!(r["lastStop"]["cause"], "no_wake");
        assert_eq!(r["lastStop"]["runId"], "r1");
        assert_eq!(r["waitingOn"]["childSources"][0]["items"][0]["id"], "c1");
        assert_eq!(r["waitingOn"]["childReports"][0]["taskId"], "t");
    }

    #[test]
    fn a_last_stop_of_another_run_is_dropped_without_invalidating_the_record() {
        let mut v = record();
        v["lastStop"]["runId"] = json!("other");
        let r = read_lifecycle_record(&v).expect("valid record");
        assert!(r.get("lastStop").is_none());
    }
}
