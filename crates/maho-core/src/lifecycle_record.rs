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
pub fn read_waits(v: &Value) -> Option<Value> {
    let children = v.get("children")?.as_number()?;
    let background=v["background"].as_array().into_iter().flatten().filter_map(|x| {let mut item=json!({"source":x.get("source")?.as_str()?,"activeCount":x.get("activeCount")?.as_number()?}); if x["retryAtMs"].is_number(){item["retryAtMs"]=x["retryAtMs"].clone();} Some(item)}).collect::<Vec<_>>();
    let human = v["human"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(read_human_wait)
        .collect::<Vec<_>>();
    Some(json!({"children":children,"background":background,"human":human}))
}
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
