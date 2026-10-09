use serde_json::Value;
use ulw_loop_core::quality_gate::validate_quality_gate;
#[test]fn documented_quality_gate_fixture_validates(){let raw=include_str!("fixtures/sample-quality-gate.json");let gate:Value=serde_json::from_str(raw).expect("fixture");let q=validate_quality_gate(&gate,None).expect("valid");assert_eq!(q.manual_qa.surface_evidence.len(),2);assert_eq!(q.gate_review.recommendation,"APPROVE");}
