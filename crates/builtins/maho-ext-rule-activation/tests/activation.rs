use maho_ext_rule_activation::{renderer::render_rule_activation_entry, types::{parse_rule_activation_details, RuleActivationDetails, Remediation}};
use maho_ext_api::{EntryRenderOptions, SessionEntry, Theme};
use serde_json::json;
fn entry(data: serde_json::Value) -> SessionEntry { SessionEntry { id: "e".into(), parent_id: None, timestamp: "0".into(), kind: "custom".into(), data } }
#[test]
fn malformed_persisted_data_returns_no_component() {
 let data = entry(json!({"kind":"project-rules","targetPath":42,"rules":[]}));
 let result = render_rule_activation_entry(&data, &EntryRenderOptions::default(), &Theme::default());
 assert!(result.is_none());
}
#[test]
fn project_rules_keep_tool_call_id() {
 let data = json!({"kind":"project-rules","targetPath":"src/a.rs","rules":["AGENTS.md"],"toolCallId":"call-1"});
 let result = parse_rule_activation_details(&data).unwrap().to_json();
 assert_eq!(result, data);
}
#[test]
fn invalid_optional_tool_call_id_is_omitted() {
 let data = json!({"kind":"project-rules","targetPath":"a","rules":["r"],"toolCallId":42});
 let result = parse_rule_activation_details(&data).unwrap().to_json();
 assert!(result.get("toolCallId").is_none());
}
#[test]
fn empty_rule_is_rejected() {
 let result = parse_rule_activation_details(&json!({"kind":"project-rules","targetPath":"a","rules":[""]}));
 assert!(result.is_none());
}
#[test]
fn unknown_remediation_is_rejected() {
 let result = parse_rule_activation_details(&json!({"kind":"ttsr","owner":"o","rules":["r"],"remediation":"unknown"}));
 assert!(result.is_none());
}
#[test]
fn ttsr_round_trip_preserves_provider_error() {
 let details = RuleActivationDetails::Ttsr { owner: "o".into(), rules: vec!["r".into()], remediation: Remediation::ProviderError };
 let result = parse_rule_activation_details(&details.to_json());
 assert_eq!(result, Some(details));
}
#[test]
fn expanded_render_includes_rule_paths() {
 let data = entry(json!({"kind":"project-rules","targetPath":"src/a.rs","rules":[".omo/rules/r.md"]}));
 let mut result = render_rule_activation_entry(&data, &EntryRenderOptions { expanded: true }, &Theme::default()).unwrap();
 assert!(result.render(80).join("\n").contains(".omo/rules/r.md"));
}
#[test]
fn collapsed_render_hides_rule_paths() {
 let data = entry(json!({"kind":"project-rules","targetPath":"src/a.rs","rules":[".omo/rules/r.md"]}));
 let mut result = render_rule_activation_entry(&data, &EntryRenderOptions { expanded: false }, &Theme::default()).unwrap();
 assert!(!result.render(80).join("\n").contains(".omo/rules/r.md"));
}
#[test]
fn registered_renderer_matches_pinned_plain_theme_reference() {
 let mut api = maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("activation", "/tmp".into(), maho_ext_api::SourceInfo::default()), maho_ext_api::ExtensionSessionProfile::default(), maho_ext_api::EventBus::default(), maho_ext_api::ExtensionRuntime::default());
 maho_ext_rule_activation::index::register_rule_activation_renderer(&mut api);
 let data = entry(json!({"kind":"project-rules","targetPath":"src/a.rs","rules":[".omo/rules/r.md"]}));
 let mut actual = Vec::new();
 for expanded in [false, true] {
  let mut result = render_rule_activation_entry(&data, &EntryRenderOptions { expanded }, &Theme::default()).unwrap();
  actual.push(json!(result.render(80)));
 }
 let invalid = entry(json!({"kind":"project-rules","targetPath":42,"rules":[".omo/rules/r.md"]}));
 assert!(render_rule_activation_entry(&invalid, &EntryRenderOptions::default(), &Theme::default()).is_none());
 actual.push(serde_json::Value::Null);
 for remediation in ["nudge", "provider-error"] {
  let data = entry(json!({"kind":"ttsr","owner":"fixture","rules":["rule-1"],"remediation":remediation}));
  let mut rendered = render_rule_activation_entry(&data, &EntryRenderOptions { expanded: true }, &Theme::default()).unwrap();
  actual.push(json!(rendered.render(80)));
 }
 let expected: serde_json::Value = serde_json::from_str(include_str!("main-hooks.golden.json")).unwrap();
 assert_eq!(json!(actual), expected);
}
