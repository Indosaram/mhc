use serde_json::Value;
pub const RULE_ACTIVATION_ENTRY_TYPE: &str = "rule-activation";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remediation { Nudge, ProviderError }
impl Remediation { pub const fn as_str(&self) -> &'static str { match self { Self::Nudge => "nudge", Self::ProviderError => "provider-error" } } }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleActivationDetails {
 ProjectRules { target_path: String, rules: Vec<String>, tool_call_id: Option<String> },
 Ttsr { owner: String, rules: Vec<String>, remediation: Remediation },
}
pub fn parse_rule_activation_details(value: &Value) -> Option<RuleActivationDetails> {
 let object = value.as_object()?;
 let rules: Vec<String> = object.get("rules")?.as_array()?.iter().map(|entry| entry.as_str().filter(|s| !s.is_empty()).map(str::to_owned)).collect::<Option<_>>()?;
 if rules.is_empty() { return None; }
 match object.get("kind")?.as_str()? {
  "project-rules" => Some(RuleActivationDetails::ProjectRules { target_path: object.get("targetPath")?.as_str().filter(|s| !s.is_empty())?.into(), rules, tool_call_id: object.get("toolCallId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned) }),
  "ttsr" => Some(RuleActivationDetails::Ttsr { owner: object.get("owner")?.as_str().filter(|s| !s.is_empty())?.into(), rules, remediation: match object.get("remediation")?.as_str()? { "nudge" => Remediation::Nudge, "provider-error" => Remediation::ProviderError, _ => return None } }),
  _ => None,
 }
}
impl RuleActivationDetails {
 pub fn to_json(&self) -> Value {
  match self {
   Self::ProjectRules { target_path, rules, tool_call_id } => { let mut data = serde_json::json!({"kind":"project-rules", "targetPath":target_path, "rules":rules}); if let Some(id) = tool_call_id { data["toolCallId"] = Value::String(id.clone()); } data },
   Self::Ttsr { owner, rules, remediation } => serde_json::json!({"kind":"ttsr", "owner":owner, "rules":rules, "remediation":remediation.as_str()}),
  }
 }
}
