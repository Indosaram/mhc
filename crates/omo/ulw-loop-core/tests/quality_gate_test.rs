use ulw_loop_core::quality_gate::*;
use ulw_loop_core::quality_gate_artifacts::RealQualityGateFs;
use ulw_loop_core::surface::UlwLoopToolkitSurface;

#[test]
fn test_classify_external_authorization_blocker() {
    assert!(classify_external_authorization_blocker("missing github auth token required for api").is_some());
    assert!(classify_external_authorization_blocker("simple syntax error").is_none());
}

#[test]
fn test_validate_quality_gate_structure() {
    let gate_json = serde_json::json!({
        "manualQa": {
            "by": "main-session",
            "status": "passed",
            "evidence": "verified cli and tests pass",
            "surfaceEvidence": [
                {
                    "id": "surface-cli",
                    "criterionRef": "C001",
                    "surface": "cli",
                    "invocation": "cargo test",
                    "verdict": "passed",
                    "artifactRefs": ["art-1"]
                }
            ],
            "adversarialCases": [
                {
                    "id": "adv-1",
                    "criterionRef": "C001",
                    "description": "adv test",
                    "scenario": "edge case boundary check",
                    "expectedBehavior": "returns clean error instead of panic",
                    "verdict": "passed",
                    "artifactRefs": ["art-1"]
                }
            ],
            "artifactRefs": [
                {
                    "id": "art-1",
                    "kind": "cli-transcript",
                    "description": "test transcript",
                    "path": "test.txt"
                }
            ]
        },
        "gateReview": {
            "by": "main-session",
            "recommendation": "APPROVE",
            "reportPath": "review.md",
            "evidence": "review passed",
            "blockers": [],
            "notes": []
        },
        "iteration": {
            "fullRerun": true,
            "status": "passed",
            "rerunCommands": ["cargo test"],
            "evidence": "all tests rerun and pass"
        },
        "criteriaCoverage": {
            "totalCriteria": 1,
            "passCount": 1,
            "originalIntent": "deliver native port",
            "desiredOutcome": "clean passing tests",
            "userOutcomeReview": "all requirements met",
            "adversarialClassesCovered": ["none"]
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.txt"), "output").unwrap();
    std::fs::write(tmp.path().join("review.md"), "approved").unwrap();

    let real_fs = RealQualityGateFs;
    let opts = ValidateQualityGateOptions {
        repo_root: Some(tmp.path()),
        fs: Some(&real_fs),
        current_attempt_dir: None,
        reviewer_surface: Some(UlwLoopToolkitSurface::LazyCodex),
    };

    let result = validate_quality_gate(&gate_json, Some(&opts));
    let qg = match result {
        Ok(qg) => qg,
        Err(error) => panic!("validation failed: {error:?}"),
    };
    assert_eq!(qg.manual_qa.status, "passed");
    assert_eq!(qg.gate_review.recommendation, "APPROVE");
}

fn valid_gate_fixture() -> serde_json::Value {
    serde_json::json!({
        "codeReview":{"by":"lazycodex-code-reviewer","recommendation":"APPROVE","codeQualityStatus":"CLEAR","reportPath":"code.md","evidence":"reviewed","blockers":[]},
        "manualQa":{"by":"lazycodex-qa-executor","status":"passed","evidence":"qa evidence","surfaceEvidence":[{"id":"surface","criterionRef":"C1","surface":"cli","invocation":"run","verdict":"passed","artifactRefs":["cli"]}],"adversarialCases":[{"id":"adv","criterionRef":"C2","scenario":"malformed","expectedBehavior":"reject","verdict":"passed","artifactRefs":["log"]}],"artifactRefs":[{"id":"cli","kind":"cli-transcript","description":"cli","path":"cli.txt"},{"id":"log","kind":"log","description":"log","path":"log.txt"}]},
        "gateReview":{"by":"lazycodex-gate-reviewer","recommendation":"APPROVE","reportPath":"gate.md","evidence":"approved","blockers":[]},"iteration":{"fullRerun":true,"status":"passed","rerunCommands":["test"],"evidence":"rerun"},"criteriaCoverage":{"totalCriteria":2,"passCount":2,"originalIntent":"intent","desiredOutcome":"outcome","userOutcomeReview":"user review","adversarialClassesCovered":["malformed_input"]}
    })
}
fn gate_error(value: serde_json::Value) -> ulw_loop_core::UlwLoopError { match ulw_loop_core::quality_gate::validate_quality_gate(&value, None) { Ok(_) => panic!("expected quality gate validation error"), Err(error) => error } }

#[test] fn quality_gate_fixture_has_five_sections() { let g=valid_gate_fixture(); for k in ["codeReview","manualQa","gateReview","iteration","criteriaCoverage"] { assert!(g.get(k).is_some()); } }
#[test] fn quality_gate_fixture_fs_variant_is_structurally_valid() { let g=valid_gate_fixture(); assert!(g["manualQa"]["artifactRefs"].as_array().unwrap().len()==2); }
#[test] fn quality_gate_missing_surface_evidence_fails_closed() { let mut g=valid_gate_fixture(); g["manualQa"]["surfaceEvidence"]=serde_json::json!([]); assert!(gate_error(g).message.contains("manualQa.surfaceEvidence")); }
#[test] fn quality_gate_unresolved_artifact_is_rejected() { let mut g=valid_gate_fixture(); g["manualQa"]["surfaceEvidence"][0]["artifactRefs"]=serde_json::json!(["missing-artifact"]); assert!(gate_error(g).message.contains("missing-artifact")); }
#[test] fn quality_gate_incompatible_kind_is_rejected() { let mut g=valid_gate_fixture(); g["manualQa"]["artifactRefs"][0]["kind"]=serde_json::json!("http-dump"); assert!(gate_error(g).message.contains("incompatible")); }
#[test] fn quality_gate_unsupported_kind_lists_guidance() { let mut g=valid_gate_fixture(); g["manualQa"]["artifactRefs"][0]["kind"]=serde_json::json!("review-report"); assert!(gate_error(g).message.contains("supported artifact kind")); }
#[test] fn quality_gate_unsupported_surface_lists_allowed_values() { let mut g=valid_gate_fixture(); g["manualQa"]["surfaceEvidence"][0]["surface"]=serde_json::json!("terminal"); assert!(gate_error(g).message.contains("supported manual QA surface")); }
#[test] fn quality_gate_placeholder_is_rejected() { let mut g=valid_gate_fixture(); g["manualQa"]["evidence"]=serde_json::json!("todo"); assert!(gate_error(g).message.contains("placeholder")); }
#[test] fn quality_gate_blockers_reject_approval() { let mut g=valid_gate_fixture(); g["gateReview"]["blockers"]=serde_json::json!(["missing"]); assert!(gate_error(g).message.contains("gateReview.blockers")); }
#[test] fn quality_gate_requires_full_rerun() { let mut g=valid_gate_fixture(); g["iteration"]["fullRerun"]=serde_json::json!(false); assert!(gate_error(g).message.contains("iteration.fullRerun")); }
#[test] fn quality_gate_not_applicable_adversarial_requires_reason() { let mut g=valid_gate_fixture(); g["manualQa"]["adversarialCases"][0]["verdict"]=serde_json::json!("not_applicable"); assert!(gate_error(g).message.contains("reason")); }
#[test] fn quality_gate_criteria_must_all_pass() { let mut g=valid_gate_fixture(); g["criteriaCoverage"]["passCount"]=serde_json::json!(1); assert!(gate_error(g).message.contains("criteriaCoverage.passCount")); }
#[test] fn quality_gate_requires_outcome_review() { let mut g=valid_gate_fixture(); g["criteriaCoverage"]["userOutcomeReview"]=serde_json::json!(""); assert!(gate_error(g).message.contains("userOutcomeReview")); }
#[test] fn quality_gate_watch_status_is_accepted() { let mut g=valid_gate_fixture(); g["codeReview"]["codeQualityStatus"]=serde_json::json!("WATCH"); assert!(ulw_loop_core::quality_gate::validate_quality_gate(&g,None).is_ok()); }
#[test] fn quality_gate_block_status_is_rejected() { let mut g=valid_gate_fixture(); g["codeReview"]["codeQualityStatus"]=serde_json::json!("BLOCK"); assert!(gate_error(g).message.contains("codeQualityStatus")); }
#[test] fn quality_gate_reasoned_not_applicable_is_accepted() { let mut g=valid_gate_fixture(); g["manualQa"]["adversarialCases"][0]["verdict"]=serde_json::json!("not_applicable"); g["manualQa"]["adversarialCases"][0]["reason"]=serde_json::json!("doc-only change"); assert!(ulw_loop_core::quality_gate::validate_quality_gate(&g,None).is_ok()); }
#[test] fn quality_gate_reasonless_not_applicable_is_rejected() { let mut g=valid_gate_fixture(); g["manualQa"]["adversarialCases"][0]["verdict"]=serde_json::json!("not_applicable"); assert!(gate_error(g).message.contains("reason")); }
#[test] fn quality_gate_surface_not_applicable_is_rejected() { let mut g=valid_gate_fixture(); g["manualQa"]["surfaceEvidence"][0]["verdict"]=serde_json::json!("not_applicable"); assert!(gate_error(g).message.contains("not_applicable")); }
#[test] fn quality_gate_attempt_fixture_contains_artifacts() { let g=valid_gate_fixture(); assert_eq!(g["manualQa"]["artifactRefs"].as_array().unwrap().len(),2); }
#[test] fn quality_gate_attempt_outside_path_is_rejected() {
    assert!(ulw_loop_core::quality_gate::validate_quality_gate(&valid_gate_fixture(), None).is_ok());
    let tmp = tempfile::tempdir().unwrap();
    for name in ["code.md", "cli.txt", "log.txt", "gate.md"] { std::fs::write(tmp.path().join(name), "x").unwrap(); }
    let opts = ValidateQualityGateOptions { repo_root: Some(tmp.path()), fs: Some(&RealQualityGateFs), current_attempt_dir: Some("attempt"), reviewer_surface: None };
    assert!(validate_quality_gate(&valid_gate_fixture(), Some(&opts)).is_err());
}
#[test] fn quality_gate_sibling_attempt_prefix_is_rejected() {
    let tmp = tempfile::tempdir().unwrap(); let root = tmp.path().join("attempt"); std::fs::create_dir_all(&root).unwrap();
    for name in ["code.md", "cli.txt", "log.txt", "gate.md"] { std::fs::write(tmp.path().join(name), "x").unwrap(); }
    let opts = ValidateQualityGateOptions { repo_root: Some(tmp.path()), fs: Some(&RealQualityGateFs), current_attempt_dir: Some("attempt"), reviewer_surface: None };
    assert!(validate_quality_gate(&valid_gate_fixture(), Some(&opts)).is_err());
}
