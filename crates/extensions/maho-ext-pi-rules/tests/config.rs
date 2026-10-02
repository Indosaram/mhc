use maho_ext_pi_rules::{config::{config_from_values, config_from_environment}, rules::types::{EnabledSources, Mode}};
fn from(key: &str, value: &str) -> maho_ext_pi_rules::rules::types::PiRulesConfig {
    config_from_values(|name| (name == key).then(|| value.to_owned()))
}
#[test] fn empty_env_defaults() { let r=config_from_values(|_|None); assert!(!r.disabled); assert_eq!(r.mode,Mode::Both); assert_eq!(r.max_rule_chars,12000); assert_eq!(r.max_result_chars,40000); assert_eq!(r.enabled_sources,EnabledSources::Auto); }
#[test] fn truthy_disabled() { let values=["1","true","yes","on"," TRUE "]; let results:Vec<_>=values.iter().map(|v|from("PI_RULES_DISABLED",v).disabled).collect(); assert_eq!(results,[true;5]); }
#[test] fn nontruthy_disabled() { let values=["0","false","","   ","maybe"]; let results:Vec<_>=values.iter().map(|v|from("PI_RULES_DISABLED",v).disabled).collect(); assert_eq!(results,[false;5]); }
#[test] fn rule_budget_override() { let r=from("PI_RULES_MAX_RULE_CHARS","50"); assert_eq!(r.max_rule_chars,50); assert_eq!(r.max_result_chars,40000); }
#[test] fn result_budget_override() { let r=from("PI_RULES_MAX_RESULT_CHARS","200"); assert_eq!(r.max_rule_chars,12000); assert_eq!(r.max_result_chars,200); }
#[test] fn invalid_numbers_keep_defaults() { let values=["abc","0","-5","","   "]; let results:Vec<_>=values.iter().map(|v|config_from_values(|_|Some((*v).into()))).collect(); assert!(results.iter().all(|r| r.max_rule_chars==12000 && r.max_result_chars==40000)); }
#[test] fn malformed_numbers_keep_defaults() { let values=["1.5","50abc","1e3","+50","0x10"]; let results:Vec<_>=values.iter().map(|v|config_from_values(|_|Some((*v).into()))).collect(); assert!(results.iter().all(|r| r.max_rule_chars==12000 && r.max_result_chars==40000)); }
#[test] fn whitespace_trimmed() { let r=from("PI_RULES_MAX_RULE_CHARS"," 50 "); assert_eq!(r.max_rule_chars,50); }
#[test] fn process_env_is_read_in_isolated_child() {
    let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","environment_child","--nocapture"]).env("PI_RULES_MAX_RULE_CHARS","77").env("PI_RULES_CONFIG_CHILD","1").output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
}
#[test] fn environment_child() { if std::env::var("PI_RULES_CONFIG_CHILD").ok().as_deref()==Some("1") { let r=config_from_environment(); assert_eq!(r.max_rule_chars,77); } }
