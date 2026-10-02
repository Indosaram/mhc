use maho_ext_bash_timeout::{apply_bash_timeout, resolve_bash_timeout_defaults};
use serde_json::json;

#[test]
fn defaults_when_absent() { assert_eq!(resolve_bash_timeout_defaults(None,None).default_seconds,1800); }
#[test]
fn maximum_when_absent() { assert_eq!(resolve_bash_timeout_defaults(None,None).max_seconds,1800); }
#[test]
fn default_when_configured() { assert_eq!(resolve_bash_timeout_defaults(Some("30"),None).default_seconds,30); }
#[test]
fn maximum_when_configured() { assert_eq!(resolve_bash_timeout_defaults(None,Some("3600")).max_seconds,3600); }
#[test]
fn default_when_invalid() { for value in ["garbage","0","-1"] { assert_eq!(resolve_bash_timeout_defaults(Some(value),None).default_seconds,1800); } }
#[test]
fn maximum_when_invalid() { for value in ["garbage","0"] { assert_eq!(resolve_bash_timeout_defaults(None,Some(value)).max_seconds,1800); } }
#[test]
fn maximum_when_below_default() { assert_eq!(resolve_bash_timeout_defaults(Some("500"),Some("100")).max_seconds,500); }
#[test]
fn injected_when_missing() { assert_eq!(apply_bash_timeout(&json!({"command":"echo hi"}),resolve_bash_timeout_defaults(None,None)),json!({"command":"echo hi","timeout":1800})); }
#[test]
fn preserved_when_below_maximum() { let p=json!({"command":"noop","timeout":30}); assert_eq!(apply_bash_timeout(&p,resolve_bash_timeout_defaults(None,None)),p); }
#[test]
fn preserved_when_above_maximum() { let p=json!({"command":"noop","timeout":9999}); assert_eq!(apply_bash_timeout(&p,resolve_bash_timeout_defaults(None,None)),p); }
#[test]
fn preserved_when_milliseconds() { let p=json!({"command":"noop","timeout":30000}); assert_eq!(apply_bash_timeout(&p,resolve_bash_timeout_defaults(None,None)),p); }
#[test]
fn injected_when_nonpositive() { for value in [0,-5] { assert_eq!(apply_bash_timeout(&json!({"command":"noop","timeout":value}),resolve_bash_timeout_defaults(None,None))["timeout"],1800); } }
#[test]
fn immutable_when_injected() { let p=json!({"command":"noop"}); let before=p.clone(); let _result=apply_bash_timeout(&p,resolve_bash_timeout_defaults(None,None)); assert_eq!(p,before); }
