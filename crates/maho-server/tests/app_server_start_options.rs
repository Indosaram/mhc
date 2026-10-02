use maho_server::app_server::start_options::*;
use serde_json::json;
#[test]
fn start_policy_defaults_and_model_reference_preserve_provider_prefix_contract() {
    assert_eq!(requested_approval_policy(&json!({"approvalPolicy":"on-failure"})), "never");
    assert_eq!(requested_approval_policy(&json!({"approvalPolicy":"on-request"})), "on-request");
    assert_eq!(parse_model_reference("p/model", Some("p")), Some(("p","model")));
    assert_eq!(parse_model_reference("model", Some("p")), Some(("p","model")));
    assert_eq!(parse_model_reference("model", None), None);
    assert_eq!(parse_model_reference("p/", None), None);
    let models = vec![json!({"provider":"p","id":"model"})];
    assert!(requested_start_model(&json!({"model":"p/model"}), &models).is_some());
    assert!(requested_start_model(&json!({"model":"q/model"}), &models).is_none());
}
