#![allow(clippy::unwrap_used)]
use serde_json::json;
use ulw_loop_core::quality_gate::validate_quality_gate;
fn shared()->serde_json::Value{json!({"gateReview":{"by":"category:deep","recommendation":"APPROVE","evidence":"review","reportPath":"report.txt","blockers":[]},"iteration":{"fullRerun":true,"status":"passed","rerunCommands":["test"],"evidence":"rerun"},"criteriaCoverage":{"totalCriteria":1,"passCount":1,"originalIntent":"intent","desiredOutcome":"outcome","userOutcomeReview":"review","adversarialClassesCovered":["none"]},"manualQa":{"by":"main-session","status":"passed","evidence":"qa","surfaceEvidence":[],"adversarialCases":[],"artifactRefs":[{"id":"a","kind":"log","description":"x","path":"x.txt"}]}})}
fn fields(g:serde_json::Value)->Vec<String>{let e=validate_quality_gate(&g,None).unwrap_err();e.details.unwrap()["fields"].as_array().unwrap().iter().map(|x|format!("{}: {}",x["field"].as_str().unwrap(),x["message"].as_str().unwrap())).collect()}
#[test]fn invalid_artifact_kind_does_not_report_unknown_ref(){let mut g=shared();g["manualQa"]["artifactRefs"]=json!([{"id":"a","kind":"invalid-kind","description":"x","path":"x.txt"}]);let f=fields(g);assert!(f.iter().any(|x|x.starts_with("manualQa.artifactRefs[0].kind:")));assert!(!f.iter().any(|x|x.contains("references unknown artifact a")));}
#[test]fn poisoned_numeric_skips_cross_field(){let mut g=shared();g["criteriaCoverage"]["passCount"]=json!("bad");let f=fields(g);assert!(f.iter().any(|x|x.contains("passCount")&&x.contains("numeric")));assert!(!f.iter().any(|x|x.contains("cover totalCriteria")));}
#[test]fn poisoned_parent_suppresses_children(){let mut g=shared();g["manualQa"]["surfaceEvidence"]=json!([null]);g["manualQa"]["adversarialCases"]=json!([null]);let f=fields(g);assert!(f.iter().any(|x|x.starts_with("manualQa.surfaceEvidence[0]:")));assert!(!f.iter().any(|x|x.starts_with("manualQa.surfaceEvidence[0].")));}


