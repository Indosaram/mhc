//! Port of the event wording of senpi packages/ai/src/api/chatgpt-usage-limit.ts
//! (`describeChatGptUsageLimitEvent`, `describeChatGptUsageLimit`).

use serde_json::{Map, Value};

use crate::utils::diagnostics::now_ms;

pub const CHATGPT_USAGE_LIMIT_PREFIX: &str = "You have hit your ChatGPT usage limit";
const ACCOUNT_LIMIT_NEXT_STEP_LEAD: &str = "To keep going now, ";
const USAGE_PAGE: &str = "https://chatgpt.com/settings/usage";

fn reached_type_advice(kind: &str) -> Option<String> {
    Some(match kind {
        "workspace_owner_credits_depleted" => {
            format!("Your workspace is out of credits: add credits at {USAGE_PAGE}.")
        }
        "workspace_member_credits_depleted" => {
            "Your workspace is out of credits: ask your workspace owner to add more.".to_owned()
        }
        "workspace_owner_usage_limit_reached" => {
            "You hit the spend cap set for your workspace: raise it to continue.".to_owned()
        }
        "workspace_member_usage_limit_reached" => {
            "You hit the spend cap your workspace owner set: ask an owner to raise it to continue."
                .to_owned()
        }
        _ => return None,
    })
}

fn plan_advice(plan: &str) -> Option<String> {
    Some(match plan {
        "plus" => format!("Upgrade to Pro, or buy credits at {USAGE_PAGE}."),
        "pro" | "prolite" | "promax" => format!("Buy credits at {USAGE_PAGE}."),
        "free" | "go" => "Upgrade to ChatGPT Plus to continue.".to_owned(),
        "team" | "business" | "self_serve_business_usage_based" | "enterprise_cbp_usage_based" => {
            "Ask your workspace admin for more access.".to_owned()
        }
        _ => return None,
    })
}

fn header_bool(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// `describeChatGptUsageLimit`.
pub fn describe_chat_gpt_usage_limit(
    plan_type: Option<&str>,
    resets_at_seconds: Option<f64>,
    header: &dyn Fn(&str) -> Option<String>,
    now: i64,
) -> String {
    let plan = plan_type
        .map(|plan| plan.trim().to_lowercase())
        .filter(|plan| !plan.is_empty());
    let plan_label = plan
        .as_ref()
        .map(|plan| format!(" ({plan} plan)"))
        .unwrap_or_default();
    let mut sentences = vec![format!("{CHATGPT_USAGE_LIMIT_PREFIX}{plan_label}.")];
    let advice = header("x-codex-rate-limit-reached-type")
        .map(|kind| kind.trim().to_lowercase())
        .and_then(|kind| reached_type_advice(&kind))
        .or_else(|| {
            (header_bool(header("x-codex-credits-has-credits").as_deref()) == Some(true)).then(|| {
                "This account has ChatGPT credits, but ChatGPT did not apply them to this request."
                    .to_owned()
            })
        })
        .or_else(|| plan.as_deref().and_then(plan_advice));
    sentences.extend(advice);
    sentences.push(format!(
        "{ACCOUNT_LIMIT_NEXT_STEP_LEAD}add an OpenAI API key, sign in with another ChatGPT account, or switch providers."
    ));
    if let Some(resets_at) = resets_at_seconds.filter(|value| value.is_finite()) {
        let mins = ((resets_at * 1000.0 - now as f64) / 60_000.0)
            .round()
            .max(0.0) as i64;
        sentences.push(format!("Try again in ~{mins} min."));
    }
    sentences.join(" ")
}

/// `describeChatGptUsageLimitEvent`: a WebSocket/stream event whose `error` names a usage limit.
pub fn describe_chat_gpt_usage_limit_event(event: &Map<String, Value>) -> Option<String> {
    let error = event.get("error")?.as_object()?;
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .or_else(|| error.get("type").and_then(Value::as_str))?
        .to_lowercase();
    if !(code.contains("usage_limit_reached") || code.contains("usage_not_included")) {
        return None;
    }
    let headers = event.get("headers").and_then(Value::as_object);
    let header = |name: &str| -> Option<String> {
        headers?
            .iter()
            .find(|(key, _)| key.to_lowercase() == name)
            .and_then(|(_, value)| value.as_str().map(str::to_owned))
    };
    Some(describe_chat_gpt_usage_limit(
        error.get("plan_type").and_then(Value::as_str),
        error.get("resets_at").and_then(Value::as_f64),
        &header,
        now_ms(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_limit_event_is_described_and_other_events_are_not() {
        let event = json!({"error": {"code": "usage_limit_reached", "plan_type": "Plus"}});
        let text =
            describe_chat_gpt_usage_limit_event(event.as_object().expect("object")).expect("text");
        assert!(text.starts_with("You have hit your ChatGPT usage limit (plus plan)."));
        assert!(text.contains("Upgrade to Pro"));
        let other = json!({"error": {"code": "rate_limit_exceeded"}});
        assert!(describe_chat_gpt_usage_limit_event(other.as_object().expect("object")).is_none());
    }
}
