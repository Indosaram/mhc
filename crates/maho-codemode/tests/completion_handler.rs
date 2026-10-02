use maho_codemode::completion::handler::*;
use serde_json::json;

#[test] fn opts_override_only_typed_strings_and_present_schema() {
    let request = normalize_request(CompletionRequest { prompt:"p".into(),model:Some("default".into()),system:Some("base".into()),schema:None,opts:Some(json!({"model":"slow","system":false,"schema":null})) });
    assert_eq!(request.model.as_deref(),Some("slow")); assert_eq!(request.system.as_deref(),Some("base")); assert_eq!(request.schema,Some(json!(null)));
}
#[test] fn tier_validation() {
    assert_eq!(resolve_completion_tier(None).unwrap(),CompletionTier::Default);
    assert_eq!(resolve_completion_tier(Some("smol")).unwrap(),CompletionTier::Smol);
    assert_eq!(resolve_completion_tier(Some("slow")).unwrap(),CompletionTier::Slow);
    assert!(resolve_completion_tier(Some("provider/model")).is_err());
}
