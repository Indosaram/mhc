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
    if let Err(e) = &result { eprintln!("validation error: {:?}", e); }
    assert!(result.is_ok());
    let qg = result.unwrap();
    assert_eq!(qg.manual_qa.status, "passed");
    assert_eq!(qg.gate_review.recommendation, "APPROVE");
}
